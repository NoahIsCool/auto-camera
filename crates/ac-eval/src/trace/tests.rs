//! The trace format: JSON round trips, the documented field names, and the
//! lines a reader must refuse.

use super::*;

/// A record with every kind of value: a frame and none, a tracking and a
/// lost subject, a wide, a framed and a held shot, and a running mix.
fn record() -> TraceRecord {
    let cam = SourceId("cam".into());
    TraceRecord {
        version: TRACE_VERSION,
        tick: 7,
        pts: Duration::from_nanos(200_000_001),
        late: true,
        skipped: 3,
        sources: vec![
            SourceTrace {
                id: cam.clone(),
                frame: Some(FrameTag {
                    seq: 6,
                    pts: Duration::from_nanos(199_999_999),
                }),
                subjects: vec![
                    SubjectTrace {
                        id: SubjectId(1),
                        query: QueryId("person".into()),
                        // Not exactly representable: the round trip must
                        // still give back the very same numbers.
                        bbox: Rect::new(0.1, 0.2, 1.0 / 3.0, 0.123_456_789_012_345_67),
                        state: TrackState::Tracking,
                    },
                    SubjectTrace {
                        id: SubjectId(4),
                        query: QueryId("guitar".into()),
                        bbox: Rect::new(0.6, 0.5, 0.2, 0.3),
                        state: TrackState::Lost,
                    },
                ],
            },
            SourceTrace {
                id: SourceId("side".into()),
                frame: None,
                subjects: Vec::new(),
            },
        ],
        shots: vec![
            ShotTrace {
                id: ShotId("cam/wide".into()),
                source: cam.clone(),
                subject: None,
                target: Some(Rect::FULL),
                crop: Rect::FULL,
            },
            ShotTrace {
                id: ShotId("cam/1/medium".into()),
                source: cam.clone(),
                subject: Some(SubjectId(1)),
                target: Some(Rect::new(0.05, 0.1, 0.4, 0.4)),
                crop: Rect::new(0.050_000_000_000_000_01, 0.1, 0.41, 0.41),
            },
            ShotTrace {
                id: ShotId("cam/4/full".into()),
                source: cam,
                subject: Some(SubjectId(4)),
                target: None,
                crop: Rect::new(0.5, 0.4, 0.4, 0.4),
            },
        ],
        switcher: SwitcherTrace {
            program: Some(ShotId("cam/wide".into())),
            preview: Some(ShotId("cam/1/medium".into())),
            transition: Some(TransitionTrace {
                base: Some(ShotId("cam/wide".into())),
                overlay: 0.156_25,
                dip: 0.0,
            }),
        },
    }
}

#[test]
fn a_record_survives_a_json_round_trip() {
    let r = record();
    let line = r.to_json_line();
    assert_eq!(TraceRecord::from_json_line(&line), Ok(r.clone()));
    // Read and written again, the line is unchanged byte for byte.
    let again = TraceRecord::from_json_line(&line).unwrap().to_json_line();
    assert_eq!(again, line);
    // A record between transitions, with nothing on air yet.
    let idle = TraceRecord {
        switcher: SwitcherTrace {
            program: None,
            preview: None,
            transition: None,
        },
        ..r
    };
    assert_eq!(TraceRecord::from_json_line(&idle.to_json_line()), Ok(idle));
}

#[test]
fn a_line_is_compact_json_ending_in_one_newline() {
    let line = record().to_json_line();
    assert!(line.ends_with('\n'));
    assert_eq!(line.matches('\n').count(), 1, "{line}");
    assert!(!line.contains(": "), "compact: {line}");
}

#[test]
fn field_names_are_the_documented_schema() {
    let v: serde_json::Value = serde_json::from_str(&record().to_json_line()).unwrap();
    assert_eq!(v["version"], 1);
    assert_eq!(v["tick"], 7);
    assert_eq!(v["pts_ns"], 200_000_001);
    assert_eq!(
        (v["late"].as_bool(), v["skipped"].as_u64()),
        (Some(true), Some(3))
    );
    let src = &v["sources"][0];
    assert_eq!(src["id"], "cam");
    assert_eq!(src["frame"]["seq"], 6);
    assert_eq!(src["frame"]["pts_ns"], 199_999_999);
    assert!(v["sources"][1]["frame"].is_null());
    let subject = &src["subjects"][1];
    assert_eq!(
        (&subject["id"], &subject["query"]),
        (&4.into(), &"guitar".into())
    );
    assert_eq!(subject["state"], "lost");
    assert_eq!(subject["bbox"]["w"], 0.2);
    let wide = &v["shots"][0];
    assert_eq!(
        (&wide["id"], &wide["source"]),
        (&"cam/wide".into(), &"cam".into())
    );
    assert!(wide["subject"].is_null());
    assert_eq!(wide["target"]["h"], 1.0);
    assert!(v["shots"][2]["target"].is_null());
    assert_eq!(v["shots"][1]["subject"], 1);
    assert_eq!(v["shots"][1]["crop"]["w"], 0.41);
    let sw = &v["switcher"];
    assert_eq!(
        (&sw["program"], &sw["preview"]),
        (&"cam/wide".into(), &"cam/1/medium".into())
    );
    assert_eq!(sw["transition"]["base"], "cam/wide");
    assert_eq!(sw["transition"]["overlay"], 0.156_25);
    assert_eq!(sw["transition"]["dip"], 0.0);
}

#[test]
fn the_documented_example_reads() {
    // The record of the module docs, on one line.
    let line = concat!(
        r#"{"version":1,"tick":42,"pts_ns":1366666653,"late":false,"skipped":0,"#,
        r#""sources":[{"id":"stage","frame":{"seq":41,"pts_ns":1366666666},"#,
        r#""subjects":[{"id":1,"query":"person","bbox":{"x":0.2,"y":0.3,"w":0.1,"h":0.4},"state":"tracking"}]}],"#,
        r#""shots":[{"id":"stage/wide","source":"stage","subject":null,"target":{"x":0.0,"y":0.0,"w":1.0,"h":1.0},"crop":{"x":0.0,"y":0.0,"w":1.0,"h":1.0}},"#,
        r#"{"id":"stage/1/medium","source":"stage","subject":1,"target":{"x":0.1,"y":0.25,"w":0.4,"h":0.4},"crop":{"x":0.11,"y":0.25,"w":0.41,"h":0.41}}],"#,
        r#""switcher":{"program":"stage/wide","preview":"stage/1/medium","transition":{"base":"stage/wide","overlay":0.25,"dip":0.0}}}"#,
    );
    let r = TraceRecord::from_json_line(line).unwrap();
    assert_eq!((r.tick, r.pts), (42, Duration::from_nanos(1_366_666_653)));
    assert_eq!(r.shots[1].target, Some(Rect::new(0.1, 0.25, 0.4, 0.4)));
    assert_eq!(r.to_json_line(), format!("{line}\n"));
}

#[test]
fn unknown_fields_are_ignored() {
    let mut v: serde_json::Value = serde_json::from_str(&record().to_json_line()).unwrap();
    v["added_later"] = serde_json::json!({"anything": [1, 2]});
    v["shots"][0]["added_later"] = true.into();
    let line = serde_json::to_string(&v).unwrap();
    assert_eq!(TraceRecord::from_json_line(&line), Ok(record()));
}

#[test]
fn other_versions_and_broken_lines_are_refused() {
    let mut v: serde_json::Value = serde_json::from_str(&record().to_json_line()).unwrap();
    v["version"] = 2.into();
    let err = TraceRecord::from_json_line(&v.to_string()).unwrap_err();
    assert!(err.message.contains("version 2"), "{err}");
    assert_eq!(err.line, None);
    v.as_object_mut().unwrap().remove("version");
    let err = TraceRecord::from_json_line(&v.to_string()).unwrap_err();
    assert!(err.message.contains("no format version"), "{err}");
    v["version"] = "1".into();
    assert!(
        TraceRecord::from_json_line(&v.to_string()).is_err(),
        "a string version"
    );
    for bad in [
        "",
        "{",
        "[]",
        "null",
        r#"{"version":1}"#,
        // A non-finite coordinate is written as null and does not read back.
        &record().to_json_line().replacen("0.41", "null", 1),
        // A negative time.
        &record().to_json_line().replacen("200000001", "-1", 1),
        // A wrong track state.
        &record().to_json_line().replacen("\"lost\"", "\"gone\"", 1),
    ] {
        assert!(TraceRecord::from_json_line(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn a_trace_is_read_line_by_line_with_line_numbers_in_errors() {
    let first = record();
    let second = TraceRecord {
        tick: 8,
        pts: Duration::from_nanos(233_333_334),
        late: false,
        skipped: 0,
        ..record()
    };
    let text = first.to_json_line() + &second.to_json_line();
    assert_eq!(parse_trace(&text), Ok(vec![first.clone(), second.clone()]));
    // The final newline may be missing; an empty trace has no records.
    assert_eq!(parse_trace(text.trim_end()).unwrap().len(), 2);
    assert_eq!(parse_trace(""), Ok(Vec::new()));
    // A torn last line, as after a crash mid-write, names its line.
    let torn = format!("{}{}", text, &second.to_json_line()[..40]);
    let err = parse_trace(&torn).unwrap_err();
    assert_eq!(err.line, Some(3));
    assert!(err.to_string().starts_with("trace line 3: "), "{err}");
    // So does a blank line in the middle.
    let blank = format!("{}\n{}", first.to_json_line(), second.to_json_line());
    assert_eq!(parse_trace(&blank).unwrap_err().line, Some(2));
}

#[test]
fn times_are_exact_nanoseconds_and_saturate() {
    let mut r = record();
    r.pts = Duration::new(86_400 * 365, 123_456_789);
    let back = TraceRecord::from_json_line(&r.to_json_line()).unwrap();
    assert_eq!(back.pts, r.pts);
    r.pts = Duration::MAX;
    let v: serde_json::Value = serde_json::from_str(&r.to_json_line()).unwrap();
    assert_eq!(v["pts_ns"], u64::MAX);
}
