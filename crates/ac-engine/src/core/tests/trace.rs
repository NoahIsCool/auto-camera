//! The per-tick trace record: what it says about a tick, and that equal
//! inputs give byte-identical traces.

use ac_eval::TraceRecord;
use ac_eval::trace::TransitionTrace;
use ac_switcher::{Transition, TransitionKind};

use super::*;

/// The JSON line of every tick of a 240-tick run of three subjects with a
/// cut, a mix and a dip, run by the operator.
fn scripted_trace() -> Vec<String> {
    let mut rig = Rig::new(config(3));
    let mut lines = Vec::new();
    for tick in 1..=240 {
        match tick {
            31 => preview_and_cut(&mut rig, "cam/wide"),
            60 => {
                rig.apply(EngineCommand::SetPreview {
                    shot: shot("cam/1/medium"),
                })
                .unwrap();
                rig.auto().unwrap();
            }
            120 => {
                rig.apply(EngineCommand::SetTransition {
                    transition: Transition {
                        kind: TransitionKind::Dip,
                        duration: 0.5,
                    },
                })
                .unwrap();
                rig.apply(EngineCommand::SetPreview {
                    shot: shot("cam/2/close"),
                })
                .unwrap();
                rig.auto().unwrap();
            }
            _ => {}
        }
        rig.step();
        lines.push(rig.core.trace_record().to_json_line());
    }
    lines
}

#[test]
fn two_identical_runs_give_byte_identical_traces() {
    let (a, b) = (scripted_trace(), scripted_trace());
    assert_eq!(a.len(), 240);
    assert!(a == b, "the traces differ");
    // The run is worth comparing: it read back, ticks are numbered in
    // order, subjects were framed and both transitions ran.
    let records = ac_eval::parse_trace(&a.concat()).unwrap();
    let ticks: Vec<u64> = records.iter().map(|r| r.tick).collect();
    assert_eq!(ticks, (1..=240).collect::<Vec<u64>>());
    assert!(records.windows(2).all(|w| w[0].pts < w[1].pts));
    let last = records.last().unwrap();
    assert!(last.sources[0].subjects.len() >= 3);
    assert!(last.shots.iter().filter(|s| s.subject.is_some()).count() >= 6);
    let mixes = records
        .iter()
        .filter_map(|r| r.switcher.transition.as_ref())
        .filter(|t| t.overlay > 0.0)
        .count();
    let dips = records
        .iter()
        .filter_map(|r| r.switcher.transition.as_ref())
        .filter(|t| t.dip > 0.0)
        .count();
    assert!(mixes > 10 && dips > 5, "{mixes} mix and {dips} dip ticks");
    assert_eq!(last.switcher.program, Some(shot("cam/2/close")));
}

#[test]
fn a_record_describes_the_tick_just_run() {
    let mut rig = Rig::new(config(2));
    rig.run(31);
    preview_and_cut(&mut rig, "cam/wide");
    rig.apply(EngineCommand::SetPreview {
        shot: shot("cam/1/medium"),
    })
    .unwrap();
    rig.run(20);
    let fed = rig.frame();
    rig.step();
    let r = rig.core.trace_record();
    let snap = rig.snapshot();
    assert_eq!(r.version, ac_eval::TRACE_VERSION);
    assert_eq!((r.tick, r.pts), (snap.tick, snap.media_time));
    assert_eq!(
        (r.late, r.skipped),
        (false, 0),
        "clock facts are the runtime's"
    );
    let src = &r.sources[0];
    assert_eq!(src.id, cam());
    let frame = src.frame.unwrap();
    assert_eq!((frame.seq, frame.pts), (fed.seq, fed.pts));
    let subjects: Vec<(SubjectId, Rect, TrackState)> = src
        .subjects
        .iter()
        .map(|s| (s.id, s.bbox, s.state))
        .collect();
    let expected: Vec<(SubjectId, Rect, TrackState)> = snap
        .subjects
        .iter()
        .map(|s| (s.id, s.bbox, s.state))
        .collect();
    assert_eq!(subjects, expected);
    assert!(
        src.subjects
            .iter()
            .all(|s| s.query == QueryId("person".into()))
    );
    let ids: Vec<&ShotId> = r.shots.iter().map(|s| &s.id).collect();
    let snap_ids: Vec<&ShotId> = snap.shots.iter().map(|s| &s.id).collect();
    assert_eq!(ids, snap_ids);
    for (t, v) in r.shots.iter().zip(&snap.shots) {
        assert_eq!(
            (&t.source, t.subject, t.crop),
            (&v.source, v.subject, v.crop)
        );
    }
    let wide = &r.shots[0];
    assert_eq!(wide.target, Some(wide.crop), "a wide aims at its crop");
    let medium = r
        .shots
        .iter()
        .find(|s| s.id == shot("cam/1/medium"))
        .unwrap();
    let target = medium.target.expect("a tracking subject's shot aims");
    // It frames its subject, centred across the subject's box; the crop
    // follows it but lags while the subject moves.
    let bbox = src.subjects[0].bbox;
    assert!(
        (bbox.x..bbox.x + bbox.w).contains(&target.center().x),
        "{target:?} {bbox:?}"
    );
    assert_eq!(r.switcher.program, Some(shot("cam/wide")));
    assert_eq!(r.switcher.preview, Some(shot("cam/1/medium")));
    assert_eq!(r.switcher.transition, None);
}

#[test]
fn a_record_before_the_first_tick_is_tick_zero() {
    let rig = Rig::new(config(1));
    let r = rig.core.trace_record();
    assert_eq!((r.tick, r.pts), (0, Duration::ZERO));
    assert_eq!(r.sources[0].frame, None);
    assert!(r.sources[0].subjects.is_empty());
    let ids: Vec<&ShotId> = r.shots.iter().map(|s| &s.id).collect();
    assert_eq!(ids, [&shot("cam/wide")]);
    assert_eq!(r.shots[0].target, Some(r.shots[0].crop));
    assert_eq!((&r.switcher.program, &r.switcher.preview), (&None, &None));
}

/// The transition of every tick while `rig` runs `ticks` ticks.
fn transitions(rig: &mut Rig, ticks: usize) -> Vec<Option<TransitionTrace>> {
    (0..ticks)
        .map(|_| {
            rig.step();
            rig.core.trace_record().switcher.transition
        })
        .collect()
}

#[test]
fn a_transition_shows_how_far_it_has_got() {
    let mut rig = Rig::new(config(1));
    rig.run(31);
    preview_and_cut(&mut rig, "cam/wide");
    let medium = shot("cam/1/medium");
    rig.apply(EngineCommand::SetPreview {
        shot: medium.clone(),
    })
    .unwrap();
    rig.auto().unwrap();
    // The default mix: 0.8 s, 24 ticks.
    let mix = transitions(&mut rig, 30);
    let running: Vec<&TransitionTrace> = mix.iter().flatten().collect();
    assert!((20..=24).contains(&running.len()), "{}", running.len());
    assert!(
        running
            .iter()
            .all(|t| t.base == Some(shot("cam/wide")) && t.dip == 0.0)
    );
    assert!(running.windows(2).all(|w| w[0].overlay < w[1].overlay));
    assert!(running.iter().all(|t| t.overlay > 0.0 && t.overlay < 1.0));
    assert_eq!(mix.last(), Some(&None), "over once program is the new shot");
    assert_eq!(
        rig.core.trace_record().switcher.program,
        Some(medium.clone())
    );

    rig.apply(EngineCommand::SetTransition {
        transition: Transition {
            kind: TransitionKind::Dip,
            duration: 0.6,
        },
    })
    .unwrap();
    rig.auto().unwrap();
    let dip = transitions(&mut rig, 24);
    let running: Vec<&TransitionTrace> = dip.iter().flatten().collect();
    assert!(running.iter().all(|t| t.overlay == 0.0));
    // Down to black over the old shot, then up from black on the new one.
    let turn = running
        .iter()
        .position(|t| t.base == Some(shot("cam/wide")))
        .expect("the new shot comes in");
    assert!(
        running[..turn]
            .iter()
            .all(|t| t.base == Some(medium.clone()))
    );
    assert!(running[..turn].windows(2).all(|w| w[0].dip < w[1].dip));
    assert!(running[turn..].windows(2).all(|w| w[0].dip > w[1].dip));
    assert!(running[turn].dip > 0.8, "{}", running[turn].dip);
    assert_eq!(dip.last(), Some(&None));
}

#[test]
fn a_lost_subject_has_no_target_and_its_crop_holds() {
    let mut c = config(1);
    c.subjects.lost_after_s = 0.5;
    c.subjects.retire_after_s = 60.0;
    let pattern = SyntheticPattern::new(W, H, FPS, 1).unwrap();
    let mut truth = synthetic_detector(pattern, 0.9);
    let gone = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let g = gone.clone();
    let detector = ScriptedDetector::from_fn("vanishing", move |frame, queries| {
        if g.load(std::sync::atomic::Ordering::SeqCst) {
            Ok(Vec::new())
        } else {
            truth.detect(frame, queries)
        }
    });
    let mut rig = Rig::new(c).with_detector(detector);
    rig.run(31);
    preview_and_cut(&mut rig, "cam/1/medium");
    gone.store(true, std::sync::atomic::Ordering::SeqCst);
    rig.picture = Some(SyntheticPattern::new(W, H, FPS, 0).unwrap());
    rig.run_secs(2.0);
    let before = rig.core.trace_record();
    rig.step();
    let r: TraceRecord = rig.core.trace_record();
    let subject = &r.sources[0].subjects[0];
    assert_eq!(
        (subject.id, subject.state),
        (SubjectId(1), TrackState::Lost)
    );
    let held = |r: &TraceRecord| {
        r.shots
            .iter()
            .find(|s| s.id == shot("cam/1/medium"))
            .cloned()
            .expect("the on-air shot is kept")
    };
    assert_eq!(held(&r).target, None);
    // The framer holds: the crop only settles where it was going.
    let (now, then) = (held(&r).crop.center(), held(&before).crop.center());
    assert!(
        (now.x - then.x).hypot(now.y - then.y) < 0.01,
        "{now:?} {then:?}"
    );
    // The wide still aims at its crop.
    assert_eq!(r.shots[0].target, Some(r.shots[0].crop));
}
