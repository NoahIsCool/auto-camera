//! The `[switcher.settle]` table: settle-before-take (spec V6, C.4).
//!
//! An operator's Cut, Auto or Accept can wait for a preview crop that is
//! still travelling to its subject, at most `max_wait_s`, and a second
//! press takes it at once; [`crate::settle`] has the rules. This table says
//! whether they apply and with which numbers:
//!
//! ```toml
//! [switcher.settle]
//! enabled = false    # off by default: every take is immediate
//! threshold = 0.6    # settledness in (0, 1] at which preview counts as settled
//! max_wait_s = 0.5   # longest wait, in (0, 0.5] seconds
//! ```
//!
//! It is off by default until the spec's open question 4 (who operates: a
//! volunteer, or an AV-literate operator) is answered: a take that does not
//! happen at once is a surprise to someone who does not know why.
//!
//! The longest wait is the spec's (C.4: a take is delayed at most 0.5 s),
//! so `max_wait_s` can only shorten it. The default threshold sits below
//! what a shot at rest reaches with the default framing: the framer holds
//! still while its ideal crop stays inside its dead zones (8% of the
//! crop's width across, 5% of its height up and down, 7% in size;
//! [`ac_framing::FramerConfig`]), so a resting shot's settledness is at
//! least `1 - 0.08 / 0.25 = 0.68` ([`crate::catalog::SETTLE_TOLERANCE`]),
//! and only a shot still on its way makes a take wait. A threshold above
//! that floor can make takes of resting shots wait the whole `max_wait_s`.
//!
//! Every value is checked whether or not the rule is enabled
//! ([`SettleSettings::problems`]), so turning it on never uncovers a bad
//! number at show time.

use serde::{Deserialize, Serialize};

use crate::settle::SettleRule;

/// Longest wait, in seconds: the spec's (C.4).
pub const MAX_WAIT_S: f64 = 0.5;

/// Settle-before-take settings; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SettleSettings {
    /// Whether an operator's take waits for preview to settle (default
    /// false).
    pub enabled: bool,
    /// Settledness in `(0, 1]` at which preview counts as settled.
    pub threshold: f64,
    /// Longest wait in seconds, in `(0, 0.5]`.
    pub max_wait_s: f64,
}

impl Default for SettleSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold: 0.6,
            max_wait_s: MAX_WAIT_S,
        }
    }
}

impl SettleSettings {
    /// Checks the threshold and the wait, enabled or not, returning the
    /// first problem of [`Self::problems`].
    pub fn validate(&self) -> Result<(), String> {
        match self.problems().into_iter().next() {
            Some(problem) => Err(problem),
            None => Ok(()),
        }
    }

    /// Every out-of-range value, enabled or not: the threshold, then the
    /// wait.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !(self.threshold > 0.0 && self.threshold <= 1.0) {
            out.push(format!(
                "switcher.settle.threshold {} must be in (0, 1]",
                self.threshold
            ));
        }
        if !(self.max_wait_s > 0.0 && self.max_wait_s <= MAX_WAIT_S) {
            out.push(format!(
                "switcher.settle.max_wait_s {} must be in (0, {MAX_WAIT_S}] seconds",
                self.max_wait_s
            ));
        }
        out
    }

    /// The rule the engine core applies: `None` when disabled (every take
    /// is immediate), and when a value is out of range (which
    /// [`Self::validate`] reports first).
    pub fn rule(&self) -> Option<SettleRule> {
        (self.enabled && self.validate().is_ok()).then_some(SettleRule {
            threshold: self.threshold,
            max_wait: self.max_wait_s,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StudioConfig;

    /// A valid one-source config, followed by `extra` TOML.
    fn studio(extra: &str) -> Result<StudioConfig, String> {
        let text = format!(
            "schema = 1\n\n[[sources]]\nid = \"stage\"\nkind = \"synthetic\"\n\
             width = 64\nheight = 36\nfps = 30\n\n{extra}"
        );
        StudioConfig::parse(&text, false)
    }

    #[test]
    fn it_is_off_by_default_with_the_spec_s_half_second() {
        let config = studio("").unwrap();
        let settle = config.switcher.settle;
        assert_eq!(settle, SettleSettings::default());
        assert!(!settle.enabled);
        assert_eq!(settle.threshold, 0.6);
        assert_eq!(settle.max_wait_s, 0.5);
        assert_eq!(settle.rule(), None, "no rule: every take is immediate");
        config.validate().unwrap();
        // A `[switcher]` table without the subtable keeps the defaults too.
        let config = studio("[switcher]\ntransition = \"cut\"\n").unwrap();
        assert_eq!(config.switcher.settle, SettleSettings::default());
    }

    #[test]
    fn the_table_turns_it_on_with_its_numbers() {
        let config = studio(
            "[switcher]\nduration = 0.5\n\n[switcher.settle]\nenabled = true\n\
             threshold = 0.75\nmax_wait_s = 0.3\n",
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(config.switcher.duration, 0.5);
        assert_eq!(
            config.switcher.settle.rule(),
            Some(SettleRule {
                threshold: 0.75,
                max_wait: 0.3,
            })
        );
        // The edges of both ranges are allowed.
        let edges =
            studio("[switcher.settle]\nenabled = true\nthreshold = 1\nmax_wait_s = 0.5\n").unwrap();
        edges.validate().unwrap();
        assert!(edges.switcher.settle.rule().is_some());
    }

    #[test]
    fn values_out_of_range_are_errors_even_while_it_is_off() {
        for (key, value) in [
            ("threshold", "0"),
            ("threshold", "-0.1"),
            ("threshold", "1.01"),
            ("threshold", "nan"),
            ("max_wait_s", "0"),
            ("max_wait_s", "-1"),
            ("max_wait_s", "0.51"),
            ("max_wait_s", "inf"),
            ("max_wait_s", "nan"),
        ] {
            for enabled in [false, true] {
                let config = studio(&format!(
                    "[switcher.settle]\nenabled = {enabled}\n{key} = {value}\n"
                ))
                .unwrap();
                let err = config.validate().unwrap_err().to_string();
                assert!(
                    err.contains(&format!("switcher.settle.{key}")),
                    "{key} = {value}: {err}"
                );
                assert_eq!(config.switcher.settle.rule(), None, "{key} = {value}");
                assert!(
                    config.problems().iter().any(|p| p.contains(key)),
                    "the readiness check lists it too"
                );
            }
        }
    }

    #[test]
    fn every_bad_value_is_listed_for_the_readiness_check() {
        let config = studio(
            "[switcher]\nduration = 0\n\n[switcher.settle]\nthreshold = 2\nmax_wait_s = 1\n",
        )
        .unwrap();
        let problems = config.problems();
        for key in [
            "switcher.duration",
            "switcher.settle.threshold",
            "switcher.settle.max_wait_s",
        ] {
            assert!(
                problems.iter().any(|p| p.contains(key)),
                "{key}: {problems:?}"
            );
        }
        let first = config.validate().unwrap_err().to_string();
        assert!(first.contains("switcher.duration"), "{first}");
    }

    #[test]
    fn unknown_and_mistyped_keys_are_errors() {
        assert!(
            studio("[switcher.settle]\nenable = true\n").is_err(),
            "a typo"
        );
        assert!(
            studio("[switcher.settle]\nmax_wait = 0.4\n").is_err(),
            "no unit"
        );
        assert!(studio("[switcher.settle]\nenabled = \"yes\"\n").is_err());
        assert!(studio("[switcher.settle]\nthreshold = \"high\"\n").is_err());
    }

    #[test]
    fn a_config_written_out_reads_back_the_same() {
        let mut config = studio("").unwrap();
        config.switcher.duration = 1.5;
        config.switcher.settle = SettleSettings {
            enabled: true,
            threshold: 0.7,
            max_wait_s: 0.4,
        };
        let text = toml::to_string(&config).unwrap();
        assert!(text.contains("[switcher.settle]"), "{text}");
        // The subtable is written ahead of `transition` and `duration`; they
        // must survive it.
        let back = StudioConfig::parse(&text, false).unwrap();
        assert_eq!(back.switcher, config.switcher);
        let json = serde_json::to_string(&config).unwrap();
        let back = StudioConfig::parse(&json, true).unwrap();
        assert_eq!(back.switcher, config.switcher);
    }
}
