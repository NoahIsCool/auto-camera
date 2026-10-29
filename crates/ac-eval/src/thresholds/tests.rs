//! The thresholds file: the defaults are the specification's numbers,
//! every number in the default file says where it comes from, and broken
//! files are refused naming the key at fault.

use super::*;

/// The default file with `from` replaced by `to` (which must occur once).
fn edited(from: &str, to: &str) -> String {
    assert_eq!(DEFAULT_THRESHOLDS.matches(from).count(), 1, "{from}");
    DEFAULT_THRESHOLDS.replace(from, to)
}

#[test]
fn the_defaults_are_the_specification_numbers() {
    let t = Thresholds::default();
    assert_eq!(t.version, THRESHOLDS_VERSION);
    // C.3 framing quality and the C.4 headroom bands and joint rule.
    assert_eq!(t.framing.min_head_torso_inside, Some(0.99));
    assert_eq!(t.framing.headroom_close, [0.0, 0.04]);
    assert_eq!(t.framing.headroom_medium_close, [0.05, 0.08]);
    assert_eq!(t.framing.headroom_medium, [0.07, 0.10]);
    assert_eq!(t.framing.headroom_full, [0.08, 0.12]);
    assert_eq!(t.framing.min_joint_clearance, Some(0.04));
    // C.4 motion caps.
    assert_eq!(t.motion.max_peak_acceleration, Some(1200.0));
    assert_eq!(t.motion.max_pan_speed, Some(0.5));
    assert_eq!(t.motion.max_zoom_rate, Some(0.3));
    assert_eq!(t.motion.min_static_hold_s, Some(0.5));
    assert_eq!(t.motion.max_jitter_at_rest, Some(0.00185));
    assert!(t.motion.max_jitter_at_rest.unwrap() < 1.0 / 540.0);
    // V9 recall at IoU 0.5; C.4 auto-director constraints; C.3 no on-air
    // swaps.
    assert_eq!(t.tracking.match_iou, 0.5);
    assert_eq!(t.switching.jump_cut_iou, 0.4);
    assert_eq!(t.switching.max_jump_cuts, Some(0));
    assert_eq!(
        (t.switching.min_dwell_talk_s, t.switching.min_dwell_music_s),
        (3.0, 2.0)
    );
    assert_eq!(t.switching.max_wide_s, 10.0);
    assert_eq!(t.switching.max_on_air_swaps, Some(0));
    // M9 health guard, as the engine counts a source stale.
    assert_eq!(t.switching.source_stale_s, 1.0);
    // Numbers the specification does not give are left unchecked.
    assert_eq!(t.framing.max_lower_cut_error, None);
    assert_eq!(t.motion.max_peak_jerk, None);
    assert_eq!(t.tracking.min_recall, None);
    assert_eq!(t.tracking.min_idf1, None);
    assert_eq!(t.switching.max_time_unavailable_s, None);
}

/// Whether `line` sets a key, or would but for a leading `# `.
fn is_key(line: &str) -> bool {
    line.trim_start_matches("# ")
        .split_once(" = ")
        .is_some_and(|(k, _)| {
            !k.is_empty() && k.chars().all(|c| c.is_ascii_lowercase() || c == '_')
        })
}

#[test]
fn every_number_in_the_default_file_follows_a_comment() {
    // Each key, set or commented out, belongs to a group of keys right
    // under a comment that gives their source.
    let lines: Vec<&str> = DEFAULT_THRESHOLDS.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if !is_key(line) || line.starts_with("version") {
            continue;
        }
        let mut first = i;
        while first > 0 && is_key(lines[first - 1]) {
            first -= 1;
        }
        let above = first.checked_sub(1).map(|j| lines[j]).unwrap_or_default();
        assert!(above.starts_with("# "), "line {}: {line}", i + 1);
    }
}

/// The keys that are parameters although named like limits.
const PARAMETERS: [&str; 4] = [
    "min_dwell_talk_s",
    "min_dwell_music_s",
    "max_wide_s",
    "min_rest_s",
];

#[test]
fn a_file_with_every_limit_left_out_reads_as_unchecked() {
    let bare: String = DEFAULT_THRESHOLDS
        .lines()
        .filter(|l| {
            let limit = l.starts_with("max_") || l.starts_with("min_");
            !limit || PARAMETERS.iter().any(|p| l.starts_with(p))
        })
        .map(|l| format!("{l}\n"))
        .collect();
    let t = Thresholds::from_toml(&bare).unwrap();
    assert_eq!(t.framing.min_head_torso_inside, None);
    assert_eq!(t.motion.max_peak_acceleration, None);
    assert_eq!(t.tracking.max_time_to_acquire_s, None);
    assert_eq!(t.switching.max_jump_cuts, None);
    assert_eq!(
        (t.switching.jump_cut_iou, t.switching.max_wide_s),
        (0.4, 10.0)
    );
}

#[test]
fn broken_files_are_refused_naming_the_key() {
    let invalid = |text: &str| match Thresholds::from_toml(text) {
        Err(ThresholdsError::Invalid(e)) => e.paths().join(", "),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        invalid(&edited(
            "headroom_close = [0.0, 0.04]",
            "headroom_close = [0.05, 0.04]"
        )),
        "framing.headroom_close"
    );
    assert_eq!(
        invalid(&edited(
            "headroom_full = [0.08, 0.12]",
            "headroom_full = [-0.1, 0.12]"
        )),
        "framing.headroom_full[0]"
    );
    assert_eq!(
        invalid(&edited(
            "max_peak_acceleration = 1200.0",
            "max_peak_acceleration = -1.0"
        )),
        "motion.max_peak_acceleration"
    );
    assert_eq!(
        invalid(&edited("match_iou = 0.5", "match_iou = 0.0")),
        "tracking.match_iou"
    );
    assert_eq!(
        invalid(&edited(
            "min_head_torso_inside = 0.99",
            "min_head_torso_inside = 1.5"
        )),
        "framing.min_head_torso_inside"
    );
    assert_eq!(
        invalid(&edited("max_wide_s = 10.0", "max_wide_s = nan")),
        "switching.max_wide_s"
    );
    assert_eq!(
        invalid(&edited("source_stale_s = 1.0", "source_stale_s = 0.0")),
        "switching.source_stale_s"
    );
    // Shape errors: a misspelt limit, a missing parameter, a wrong type.
    let syntax = |text: &str| match Thresholds::from_toml(text) {
        Err(ThresholdsError::Syntax(m)) => m,
        other => panic!("{other:?}"),
    };
    assert!(syntax(&edited("max_jump_cuts = 0", "max_jump_cut = 0")).contains("max_jump_cut"));
    assert!(syntax(&edited("settle_band = 0.01\n", "")).contains("settle_band"));
    assert!(
        syntax(&edited("max_long_wides = 0", "max_long_wides = -1")).contains("max_long_wides")
    );
    assert!(matches!(
        Thresholds::from_toml("not = [toml"),
        Err(ThresholdsError::Syntax(_))
    ));
    // The version is checked first.
    assert_eq!(
        Thresholds::from_toml(&edited("version = 1", "version = 2")),
        Err(ThresholdsError::Version(2))
    );
    assert_eq!(
        Thresholds::from_toml(&edited("version = 1", "")),
        Err(ThresholdsError::NoVersion)
    );
    assert!(matches!(
        Thresholds::from_toml(&edited("version = 1", "version = \"1\"")),
        Err(ThresholdsError::Syntax(_))
    ));
    // Validation names every broken key at once.
    let mut t = Thresholds::default();
    t.motion.rest_speed = 0.0;
    t.switching.min_dwell_talk_s = -3.0;
    assert_eq!(
        t.validate().unwrap_err().paths(),
        ["motion.rest_speed", "switching.min_dwell_talk_s"]
    );
}
