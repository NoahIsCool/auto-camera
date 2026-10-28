//! Reading a config file: a studio file, or a show laid over the studio
//! file it names.
//!
//! [`read_file`] reads the file once and looks at its top-level keys: a
//! `show` key makes it a show ([`Show::is_show`]), anything else is a
//! studio file. A studio file is parsed at any schema this build reads and
//! its relative paths are resolved against its folder. A show is parsed at
//! any show schema this build reads and its own relative paths (`studio`,
//! source files) are resolved against its folder; then the studio file it
//! names is read the same way and the show is laid over it
//! ([`Show::apply_to`]). Nothing is validated here:
//! [`StudioConfig::load_with_warnings`] validates what [`read_file`]
//! returns, and the readiness check lists its problems, so a show and a
//! studio file meet the same checks.
//!
//! # Invariants
//!
//! * The studio a show names must be a studio file: a show over a show is
//!   refused, so a chain of shows, or a show naming itself, cannot loop.
//! * A show without `studio` is refused: the studio file is the base layer,
//!   and a show alone does not say what the studio is (its detector, its
//!   tuning, its encoder).
//! * Every warning starts with the path of the file it is about, so both
//!   the studio file's (a missing `schema` key) and the show's (an older
//!   show schema) reach the operator.
//! * [`LoadedConfig::studio`] is the studio file read,
//!   [`LoadedConfig::show`] says whether a show was laid over it, and
//!   [`LoadedConfig::base`] is that studio file's config before any show
//!   was laid over it.

use std::path::Path;

use super::{ConfigError, LoadedConfig, StudioConfig};
use crate::show::Show;

/// Reads and parses the studio or show file at `path`; see the module docs.
pub(super) fn read_file(path: &Path) -> Result<LoadedConfig, ConfigError> {
    let (text, json) = read_text(path)?;
    if !Show::is_show(&text, json) {
        return read_studio(path, &text, json);
    }
    let (mut show, warnings) =
        Show::parse_with_warnings(&text, json).map_err(|message| ConfigError::Parse {
            path: path.into(),
            message,
        })?;
    if let Some(dir) = path.parent() {
        show.resolve_paths(dir);
    }
    let mut loaded = open_show(&show, path)?;
    loaded.warnings.extend(named(path, warnings));
    Ok(loaded)
}

/// What opening `show` as the show file `path` gives, its paths already
/// resolved: the studio file it names, read, with the show laid over it.
/// Not validated.
pub(crate) fn open_show(show: &Show, path: &Path) -> Result<LoadedConfig, ConfigError> {
    let Some(studio) = &show.studio else {
        return Err(ConfigError::Invalid(format!(
            "{}: a show names the studio file it lies over, as in `studio = \"studio.toml\"`",
            path.display()
        )));
    };
    let (text, json) = read_text(studio)?;
    if Show::is_show(&text, json) {
        return Err(ConfigError::Invalid(format!(
            "{}: `studio` names {}, which is a show; it must name a studio file",
            path.display(),
            studio.display()
        )));
    }
    let mut loaded = read_studio(studio, &text, json)?;
    show.apply_to(&mut loaded.config);
    loaded.show = true;
    Ok(loaded)
}

/// The text of the file at `path`, and whether it is JSON (`.json`; any
/// other file is TOML).
fn read_text(path: &Path) -> Result<(String, bool), ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.into(),
        source,
    })?;
    Ok((text, path.extension().is_some_and(|e| e == "json")))
}

/// The studio file at `path`, whose text is `text`, parsed and its
/// relative paths resolved against its folder.
fn read_studio(path: &Path, text: &str, json: bool) -> Result<LoadedConfig, ConfigError> {
    let (mut config, warnings) =
        StudioConfig::parse_with_warnings(text, json).map_err(|message| ConfigError::Parse {
            path: path.into(),
            message,
        })?;
    if let Some(dir) = path.parent() {
        config.resolve_paths(dir);
    }
    Ok(LoadedConfig {
        base: config.clone(),
        config,
        warnings: named(path, warnings),
        studio: path.into(),
        show: false,
    })
}

/// `warnings`, each starting with the path of the file it is about.
fn named(path: &Path, warnings: Vec<String>) -> Vec<String> {
    warnings
        .into_iter()
        .map(|w| format!("{}: {w}", path.display()))
        .collect()
}

#[cfg(test)]
mod tests;
