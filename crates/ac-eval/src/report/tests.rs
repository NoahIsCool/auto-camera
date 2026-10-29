//! The report: verdicts at and past limits, the summary, never a NaN, and
//! the JSON form.

use super::*;

#[test]
fn limits_are_inclusive_with_a_tolerance() {
    let at_least = Limit::Min(0.99);
    assert!(at_least.admits(0.99) && at_least.admits(1.0));
    assert!(at_least.admits(0.99 - 1e-12), "within the tolerance");
    assert!(!at_least.admits(0.989));
    let at_most = Limit::Max(1200.0);
    assert!(at_most.admits(1200.0) && !at_most.admits(1200.01));
    // The tolerance scales with the limit: 1e-9 of 1200.
    assert!(at_most.admits(1200.0 + 1e-7) && !at_most.admits(1200.0 + 1e-5));
    assert!(Limit::Max(0.0).admits(0.0) && !Limit::Max(0.0).admits(1.0));
}

#[test]
fn every_metric_gets_one_verdict() {
    let m = |v: f64, limit| Metric::new("m", "u", Measure::Value(v), 1, limit).verdict;
    assert_eq!(m(1.0, Limit::max(Some(1.0))), Verdict::Pass);
    assert_eq!(m(2.0, Limit::max(Some(1.0))), Verdict::Fail);
    assert_eq!(m(0.5, Limit::min(Some(0.99))), Verdict::Fail);
    assert_eq!(m(2.0, Limit::max(None)), Verdict::Unchecked);
    let none = Metric::new(
        "m",
        "u",
        Measure::NotMeasurable("why".into()),
        0,
        Limit::max(Some(1.0)),
    );
    assert_eq!(none.verdict, Verdict::NotMeasurable);
    assert_eq!(none.value(), None);
    // A non-finite value is never reported as a number.
    for v in [f64::NAN, f64::INFINITY] {
        let bad = Metric::new("m", "u", Measure::Value(v), 1, Limit::max(Some(1.0)));
        assert_eq!(bad.verdict, Verdict::NotMeasurable);
        assert!(matches!(bad.measure, Measure::NotMeasurable(_)));
    }
    // A negative zero is written as a zero.
    let zero = Metric::new("m", "s", Measure::Value(-0.0), 0, None).detail("d", -0.0);
    assert!(zero.value().unwrap().is_sign_positive() && zero.details["d"].is_sign_positive());
    let detailed = Metric::count("c", "takes", 2, 5, Limit::max_count(Some(0)))
        .detail("kept", 1.0)
        .detail("dropped", f64::NAN);
    assert_eq!(detailed.verdict, Verdict::Fail);
    assert_eq!(detailed.details.keys().collect::<Vec<_>>(), ["kept"]);
}

/// A report with one metric of each verdict.
fn sample() -> Report {
    let metrics = vec![
        Metric::new("a.pass", "s", Measure::Value(0.5), 3, Limit::max(Some(1.0)))
            .detail("band_low", 0.0),
        Metric::count("a.fail", "takes", 1, 4, Limit::max_count(Some(0)))
            .for_query(&QueryId("pastor".into())),
        Metric::new("a.unchecked", "s", Measure::Value(2.0), 1, None),
        Metric::new(
            "a.none",
            "s",
            Measure::NotMeasurable("no labelled crown".into()),
            0,
            Limit::min(Some(0.04)),
        ),
    ];
    let coverage = Coverage {
        ticks: 10,
        keyframes: 4,
        scored_keyframes: 3,
    };
    Report::new(
        ClipId("clip".into()),
        SourceId("cam".into()),
        coverage,
        metrics,
    )
}

#[test]
fn the_report_passes_only_without_failures() {
    let r = sample();
    assert_eq!(
        r.summary,
        Summary {
            pass: 1,
            fail: 1,
            unchecked: 1,
            not_measurable: 1
        }
    );
    assert!(!r.pass);
    assert_eq!(
        r.failures().map(|m| m.name.as_str()).collect::<Vec<_>>(),
        ["a.fail"]
    );
    assert!(r.metric("a.fail", Some("pastor")).is_some());
    assert!(r.metric("a.fail", None).is_none());
    let passing = Report::new(
        ClipId("c".into()),
        SourceId("cam".into()),
        Coverage::default(),
        Vec::new(),
    );
    assert!(passing.pass);
}

#[test]
fn the_json_names_each_field_and_reads_back() {
    let r = sample();
    let json = r.to_json();
    assert!(json.ends_with("}\n"));
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["version"], REPORT_VERSION);
    assert_eq!(v["clip"], "clip");
    assert_eq!(v["pass"], false);
    assert_eq!(v["summary"]["fail"], 1);
    assert_eq!(v["coverage"]["scored_keyframes"], 3);
    let pass = &v["metrics"][0];
    assert_eq!(pass["name"], "a.pass");
    assert_eq!(pass["value"], 0.5);
    assert_eq!(pass["limit"], serde_json::json!({ "max": 1.0 }));
    assert_eq!(pass["verdict"], "pass");
    assert_eq!(pass["details"]["band_low"], 0.0);
    assert!(pass.get("query").is_none() && pass.get("not_measurable").is_none());
    assert_eq!(v["metrics"][1]["query"], "pastor");
    assert!(v["metrics"][2].get("limit").is_none());
    let none = &v["metrics"][3];
    assert_eq!(none["not_measurable"], "no labelled crown");
    assert!(none.get("value").is_none());
    assert_eq!(none["verdict"], "not_measurable");
    assert_eq!(Report::from_json(&json), Ok(r));
    assert!(Report::from_json("{}").is_err());
}
