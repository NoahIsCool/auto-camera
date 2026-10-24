//! Building the studio's detectors at engine start: one loader per source,
//! and the shared process they use when the detector runs in a sidecar.
//!
//! # Theory
//!
//! Discovery runs one worker per source, each with its own
//! [`ac_vision::Detector`] (ADR 0002). Most detectors are built per source
//! ([`build_detector`]): the synthetic one from its source's pattern, OWLv2
//! as one ONNX session per source. A sidecar detector (`kind = "sidecar"`,
//! [`SidecarSettings`]) is different: its model runs in one process for the
//! whole studio, because SAM 3.1 peaks at about 3.3 GiB and saturates the
//! GPU (measured, design §4.6). [`Detectors::build`] therefore starts that
//! process once, as an [`ac_sidecar::SidecarService`], and gives every
//! source a loader holding a non-owning [`ac_sidecar::SidecarClient`].
//! Each source still has its own detector on its own worker thread; the
//! service serves their passes one at a time, oldest first, so the queue is
//! bounded by the number of sources (ADR 0004).
//!
//! # Failing early
//!
//! A bad detector config fails `Engine::start` with the reason, as missing
//! OWL model files do: the sidecar's program must exist (a path, or a name
//! found on `PATH`), every path-like element of its command must exist
//! (the script, the model folder; see [`SidecarSettings`] for what counts
//! as a path), and the process must spawn. What can only fail once it runs
//! (a wrong `--model` makes the sidecar send a fatal error and exit, a load
//! misses `startup_timeout_s`) fails each source's loader instead, and every
//! request of that source then reports the reason, as for a model that
//! fails to load in-process.
//!
//! # Ownership
//!
//! The engine thread owns the service ([`Detectors::sidecar`]) and is the
//! only one that can end the process; loaders and detectors only queue
//! work. At shutdown the engine drops the service *before* joining the
//! workers: that interrupts a pass or a load in progress (the workers then
//! get `Unavailable` at once), closes the sidecar's stdin, waits up to 2 s
//! and kills and reaps it, so quitting never waits out a request deadline
//! and no process outlives the engine.

use std::path::Path;

use ac_sidecar::{DetectorConfig, SidecarClient, SidecarService};
use ac_vision::Detector;

use crate::config::{DetectorSettings, SidecarSettings, StudioConfig};
use crate::discovery::{DetectorLoader, build_detector};
use crate::runtime::EngineError;

/// Every source's detector loader, and the sidecar they share, if any.
pub struct Detectors {
    /// One loader per source, in config order; `None` where discovery is
    /// off.
    pub loaders: Vec<Option<DetectorLoader>>,
    /// The shared sidecar process; the engine must keep it until its
    /// workers stop (see the module docs).
    pub sidecar: Option<SidecarService>,
}

impl std::fmt::Debug for Detectors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let loaders: Vec<bool> = self.loaders.iter().map(Option::is_some).collect();
        f.debug_struct("Detectors")
            .field("loaders", &loaders)
            .field("sidecar", &self.sidecar)
            .finish()
    }
}

impl Detectors {
    /// Checks the detector settings, starts the sidecar when there is one,
    /// and makes every source's loader; see the module docs.
    pub fn build(config: &StudioConfig) -> Result<Self, EngineError> {
        let DetectorSettings::Sidecar(settings) = &config.detector else {
            let mut loaders = Vec::new();
            for entry in &config.sources {
                let loader = build_detector(&config.detector, &entry.input).map_err(|message| {
                    EngineError::Detector {
                        id: entry.id.clone(),
                        message,
                    }
                })?;
                loaders.push(loader);
            }
            return Ok(Self {
                loaders,
                sidecar: None,
            });
        };
        check_command(settings, std::env::var_os("PATH").as_deref())
            .map_err(EngineError::Sidecar)?;
        let service = SidecarService::start(settings.host_config())
            .map_err(|e| EngineError::Sidecar(e.to_string()))?;
        let detector = settings.detector_config(config.prompt_lexicon());
        let loaders = config
            .sources
            .iter()
            .map(|_| Some(sidecar_loader(service.client(), detector.clone())))
            .collect();
        Ok(Self {
            loaders,
            sidecar: Some(service),
        })
    }
}

/// A loader that waits for the sidecar's hello on the worker thread and
/// returns a detector on `client`.
fn sidecar_loader(client: SidecarClient, config: DetectorConfig) -> DetectorLoader {
    Box::new(move || {
        let detector = client.detector(config).map_err(|e| e.to_string())?;
        Ok(Box::new(detector) as Box<dyn Detector>)
    })
}

/// Checks that the sidecar's program can be found (with `path`, the value
/// of `PATH`, for a bare name) and that every path-like argument exists.
fn check_command(settings: &SidecarSettings, path: Option<&std::ffi::OsStr>) -> Result<(), String> {
    let program = settings.command.first().map_or("", String::as_str);
    if !program.contains('/') {
        let found = path
            .map(|p| std::env::split_paths(p).any(|dir| dir.join(program).is_file()))
            .unwrap_or(false);
        if !found {
            return Err(format!(
                "detector.command[0] {program:?} is not on PATH; give its path \
                 (relative to the config file, or absolute)"
            ));
        }
    }
    for (i, arg) in settings.path_args() {
        let p = Path::new(arg);
        if i == 0 && !p.is_file() {
            return Err(format!("detector.command[0]: program not found: {arg}"));
        }
        if !p.exists() {
            return Err(format!("detector.command[{i}]: not found: {arg}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_sources::SourceConfig;

    /// A one-source synthetic studio with `detector`.
    fn studio(detector: DetectorSettings) -> StudioConfig {
        let mut c = StudioConfig::with_source(
            "cam",
            SourceConfig::Synthetic {
                width: 64,
                height: 36,
                fps: 30.0,
                subjects: 1,
            },
        );
        c.detector = detector;
        c
    }

    /// This crate's directory, which exists.
    fn here() -> String {
        env!("CARGO_MANIFEST_DIR").to_owned()
    }

    #[test]
    fn programs_are_found_on_path_or_by_path() {
        let dir = here();
        let path = std::ffi::OsString::from(format!("/nonexistent:{dir}"));
        let on_path = SidecarSettings::new(["Cargo.toml", "--side", "672"]);
        check_command(&on_path, Some(&path)).unwrap();
        let msg = check_command(&on_path, None).unwrap_err();
        assert!(msg.contains("not on PATH"), "{msg}");
        let msg =
            check_command(&SidecarSettings::new(["no-such-sidecar"]), Some(&path)).unwrap_err();
        assert!(msg.contains("\"no-such-sidecar\""), "{msg}");
        let by_path = SidecarSettings::new([format!("{dir}/Cargo.toml")]);
        check_command(&by_path, None).unwrap();
        let folder = SidecarSettings::new([format!("{dir}/src")]);
        let msg = check_command(&folder, None).unwrap_err();
        assert!(msg.contains("program not found"), "{msg}");
    }

    #[test]
    fn path_like_arguments_must_exist() {
        let dir = here();
        let program = format!("{dir}/Cargo.toml");
        let ok = SidecarSettings::new([
            program.clone(),
            format!("{dir}/src"),
            "--model=missing/x".into(),
            "fake".into(),
        ]);
        check_command(&ok, None).unwrap();
        let missing = SidecarSettings::new([program, "--model".into(), format!("{dir}/missing")]);
        let msg = check_command(&missing, None).unwrap_err();
        assert!(
            msg.contains("command[2]") && msg.contains("missing"),
            "{msg}"
        );
    }

    #[test]
    fn a_bad_sidecar_command_fails_the_build_with_the_reason() {
        let c = studio(DetectorSettings::Sidecar(SidecarSettings::new([
            "/nonexistent/venv/bin/python",
            "server.py",
        ])));
        let err = Detectors::build(&c).unwrap_err();
        assert!(matches!(&err, EngineError::Sidecar(m) if m.contains("/nonexistent/venv")));
        assert!(err.to_string().starts_with("detector sidecar: "), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_sidecar_that_dies_before_hello_fails_every_loader_with_its_reason() {
        let c = studio(DetectorSettings::Sidecar(SidecarSettings {
            startup_timeout_s: 5.0,
            ..SidecarSettings::new(["sh", "-c", "echo no model here >&2; exit 3"])
        }));
        let mut built = Detectors::build(&c).unwrap();
        let service = built.sidecar.take().expect("one shared sidecar");
        let loader = built.loaders.remove(0).expect("a loader");
        let err = loader().map(|d| d.name().to_owned()).unwrap_err();
        assert!(
            err.contains("before hello") && err.contains("no model here"),
            "{err}"
        );
        let pid_gone = service.status().pid.is_none();
        drop(service);
        assert!(pid_gone, "the dead child was reaped");
    }

    #[test]
    fn per_source_detectors_need_no_sidecar() {
        let none = Detectors::build(&studio(DetectorSettings::None)).unwrap();
        assert!(none.sidecar.is_none());
        assert!(none.loaders.iter().all(Option::is_none));
        let synthetic =
            Detectors::build(&studio(DetectorSettings::Synthetic { score: 0.9 })).unwrap();
        assert!(synthetic.sidecar.is_none());
        assert_eq!(synthetic.loaders.len(), 1);
        assert!(synthetic.loaders[0].is_some());
    }
}
