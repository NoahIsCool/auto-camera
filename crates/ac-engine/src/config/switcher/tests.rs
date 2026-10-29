//! Tests for the `[switcher]` table: `start_on_wide` is off unless a
//! config turns it on, takes only a boolean, and survives a round trip;
//! the transition's checks are unchanged by it.

use super::*;
use crate::config::StudioConfig;

/// The smallest valid config, followed by `switcher`, a `[switcher]` table
/// (or nothing).
fn config_with(switcher: &str) -> String {
    format!(
        "schema = 1\n\n[[sources]]\nid = \"stage\"\nkind = \"synthetic\"\n\
         width = 640\nheight = 360\nfps = 30\n\n{switcher}"
    )
}

/// [`config_with`] parsed and validated.
fn parsed(switcher: &str) -> StudioConfig {
    let c = StudioConfig::parse(&config_with(switcher), false).unwrap();
    c.validate().unwrap();
    c
}

#[test]
fn start_on_wide_is_off_by_default() {
    assert!(!SwitcherSettings::default().start_on_wide);
    assert!(!parsed("").switcher.start_on_wide);
    // A table that names only the transition keeps the default too.
    let c = parsed("[switcher]\ntransition = \"cut\"\n");
    assert!(!c.switcher.start_on_wide);
    assert_eq!(c.switcher.transition, TransitionKind::Cut);
}

#[test]
fn start_on_wide_can_be_turned_on_beside_the_transition() {
    let c = parsed("[switcher]\nstart_on_wide = true\n");
    assert!(c.switcher.start_on_wide);
    assert_eq!(c.switcher.to_transition(), Transition::default());
    let c = parsed("[switcher]\ntransition = \"dip\"\nduration = 1.5\nstart_on_wide = true\n");
    assert!(c.switcher.start_on_wide);
    assert_eq!(c.switcher.transition, TransitionKind::Dip);
    assert_eq!(c.switcher.duration, 1.5);
    // JSON reads it the same way.
    let json = r#"{"schema": 1,
        "sources": [{"id": "stage", "kind": "synthetic", "width": 64, "height": 36, "fps": 30}],
        "switcher": {"start_on_wide": true}}"#;
    assert!(
        StudioConfig::parse(json, true)
            .unwrap()
            .switcher
            .start_on_wide
    );
}

#[test]
fn start_on_wide_takes_only_a_boolean_and_typos_are_errors() {
    for table in [
        "[switcher]\nstart_on_wide = 1\n",
        "[switcher]\nstart_on_wide = \"yes\"\n",
        "[switcher]\nstart_on_wides = true\n",
        "[switcher]\nstart_wide = true\n",
    ] {
        assert!(
            StudioConfig::parse(&config_with(table), false).is_err(),
            "accepted {table:?}"
        );
    }
}

#[test]
fn start_on_wide_survives_a_round_trip_and_leaves_the_checks_alone() {
    let mut c = parsed("[switcher]\nstart_on_wide = true\n");
    let back = StudioConfig::parse(&toml::to_string(&c).unwrap(), false).unwrap();
    assert_eq!(back.switcher, c.switcher);
    // A bad duration is still refused, whatever start_on_wide says.
    c.switcher.duration = 0.0;
    assert!(c.validate().is_err());
    c.switcher.transition = TransitionKind::Cut;
    c.validate().unwrap();
}
