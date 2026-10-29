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
//! core sends them one request at a time in turns
//! ([`DetectorSettings::is_shared`]), each with its source's newest frame,
//! and a request that still waited longer than `discovery.max_result_age_s`
//! for the process is skipped without a pass (ADR 0004).
//!
//! # Failing early
//!
//! A bad detector config fails `Engine::start` with the reason, as missing
//! OWL model files do: the sidecar's program must exist (a path, or a name
//! found on `PATH`), every path-like element of its command must exist
//! (the script, the model folder; see [`SidecarSettings`] for what counts
//! as a path), and the process must spawn. What can only fail once it runs
//! (a wrong `--model` makes the sidecar send a fatal error and exit, a load
//! misses `startup_timeout_s`) does not fail the loaders: a cold first
//! load, or one crash during load, must not turn detection off for the
//! session. Each source gets its detector anyway; its passes fail with the
//! reason (so the window shows it) while the host restarts the process
//! under its backoff (1 s, doubling up to 30 s), and work once a restart
//! says hello. Only an invalid detector configuration fails a loader.
//!
//! # Ownership
//!
//! The engine thread owns the service ([`Detectors::sidecar`]) and is the
//! only one that can end the process; loaders and detectors only queue
//! work. At shutdown the engine stops the service *before* joining the
//! workers: that interrupts a pass or a load in progress (the workers then
//! get `Unavailable` at once), closes the sidecar's stdin, waits up to 2 s
//! and kills and reaps it, so quitting never waits out a request deadline
//! and no process outlives the engine. The stopped service still reports
//! its final status, which the final snapshot carries.

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
        let detector =
            settings.detector_config(config.prompt_lexicon(), config.discovery.max_result_age());
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

/// A loader that waits for the sidecar's first hello on the worker thread
/// and returns a detector on `client`, also when that start failed (see
/// the module docs).
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
    fn a_failed_first_start_is_retried_and_its_reason_reported() {
        let c = studio(DetectorSettings::Sidecar(SidecarSettings {
            startup_timeout_s: 5.0,
            ..SidecarSettings::new(["sh", "-c", "echo no model here >&2; exit 3"])
        }));
        let mut built = Detectors::build(&c).unwrap();
        let service = built.sidecar.take().expect("one shared sidecar");
        let loader = built.loaders.remove(0).expect("a loader");
        // The start failed, but the detector exists: its passes go through
        // the host, which restarts the process after its backoff.
        let mut detector = loader().expect("a failed start is not a load failure");
        assert_eq!(detector.name(), ac_sidecar::service::UNNAMED);
        let pattern = ac_sources::SyntheticPattern::new(64, 36, 30.0, 1).unwrap();
        let queries = [ac_vision::PromptQuery::new("q", "a person")];
        let frame = pattern.frame(&ac_core::SourceId("cam".into()), 1);
        let err = detector.detect(&frame, &queries).unwrap_err().to_string();
        assert!(
            err.contains("before hello") && err.contains("no model here"),
            "{err}"
        );
        assert!(service.status().pid.is_none(), "the dead child was reaped");
        // After the 1 s backoff, a pass starts the process again.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let frame = pattern.frame(&ac_core::SourceId("cam".into()), 2);
        let err = detector.detect(&frame, &queries).unwrap_err().to_string();
        assert!(err.contains("no model here"), "{err}");
        assert_eq!(service.status().stats.spawns, 2, "restarted once");
        drop(service);
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
