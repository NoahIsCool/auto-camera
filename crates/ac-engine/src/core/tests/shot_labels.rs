//! Shot labels through the core: an alias from one stray detector pass
//! continues its subject but makes no shot, and an alias shot on air keeps
//! its id and name after its label goes stale, until it leaves the air.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ac_core::Extent;

use super::*;

/// [`config`] on a still picture of `subjects` people, with a "Speaker"
/// query framing medium shots and a "Guitarist" query framing close-ups,
/// and the ground-truth boxes of the people in the picture.
fn band(subjects: u32) -> (Rig, Vec<Rect>) {
    let mut c = config(subjects);
    let preset = |size| Some(vec![PresetSpec::Size(size)]);
    c.queries = vec![
        QuerySettings {
            presets: preset(ShotSize::Medium),
            ..QuerySettings::new("Speaker", "speaker")
        },
        QuerySettings {
            presets: preset(ShotSize::Close),
            ..QuerySettings::new("Guitarist", "guitarist")
        },
    ];
    let mut rig = Rig::new(c);
    let still = rig.pattern.frame(&cam(), 0);
    let boxes = rig
        .pattern
        .ground_truth(still.pts)
        .iter()
        .map(|g| g.bbox)
        .collect();
    rig.still = Some(still);
    (rig, boxes)
}

/// A whole-person detection of `query` at `bbox`.
fn found(query: &str, bbox: Rect) -> Detection {
    Detection {
        query: QueryId(query.into()),
        bbox,
        score: 0.9,
        extent: Extent::Person,
    }
}

/// The shot `id`'s name, if it exists.
fn name(rig: &Rig, id: &str) -> Option<String> {
    rig.core
        .catalog()
        .get(&shot(id))
        .map(|e| e.shot.name.clone())
}

/// A guitarist, boxed by both prompts, and an audience member, boxed by
/// the speaker prompt; in pass 20 people-first matching hands the guitar
/// box to the audience member instead. The audience member answers
/// "guitarist" from then on, but never gets a guitarist close-up, while
/// the real guitarist keeps theirs.
#[test]
fn one_stray_guitarist_pass_adds_no_guitarist_shot() {
    let (rig, boxes) = band(2);
    let (g, a) = (boxes[0], boxes[1]);
    assert!(g.iou(&a) < 0.3, "two separate people");
    let mut passes = 0;
    let detector = ScriptedDetector::from_fn("band", move |_, _| {
        passes += 1;
        let guitar = if passes == 20 { a } else { g };
        Ok(vec![
            found("guitarist", guitar),
            found("speaker", g),
            found("speaker", a),
        ])
    });
    let mut rig = rig.with_detector(detector);
    // "guitarist" sorts first, so it admits the guitarist as subject 1 and
    // the speaker box aliases on; the audience member is subject 2.
    let mut saw_guitarist_close = false;
    for _ in 0..(60.0 * FPS) as usize {
        rig.step();
        assert_eq!(name(&rig, "cam/2/close"), None, "a stray close-up");
        saw_guitarist_close |= name(&rig, "cam/1/close").is_some();
    }
    assert!(saw_guitarist_close);
    assert_eq!(
        name(&rig, "cam/1/close").as_deref(),
        Some("Guitarist 1 Close")
    );
    assert_eq!(
        name(&rig, "cam/2/medium").as_deref(),
        Some("Speaker 2 Medium")
    );
    let store = rig.core.store(0).unwrap();
    assert_eq!(store.len(), 2);
    assert!(rig.snapshot().sources[0].discovery.applied >= 55);
    let audience = store.record(SubjectId(2)).unwrap();
    assert!(
        audience.answers(&QueryId("guitarist".into())),
        "the stray pass aliased"
    );
}

/// A speaker who plays guitar for a while: the guitarist close-up is cut
/// to program, then the guitar is put down and the guitarist label goes
/// stale. On air the shot keeps its id and its name (it is not renamed
/// after the speaker query, which still frames the subject); once it
/// leaves program and preview it goes.
#[test]
fn an_on_air_alias_shot_keeps_its_id_and_name_until_it_leaves_air() {
    let (rig, boxes) = band(1);
    let body = boxes[0];
    let playing = Arc::new(AtomicBool::new(true));
    let guitar = playing.clone();
    let mut passes = 0;
    let detector = ScriptedDetector::from_fn("player", move |_, _| {
        passes += 1;
        let mut found_now = vec![found("speaker", body)];
        // The first pass sees only the speaker, who becomes the primary.
        if passes > 1 && guitar.load(Ordering::SeqCst) {
            found_now.push(found("guitarist", body));
        }
        Ok(found_now)
    });
    let mut rig = rig.with_detector(detector);
    rig.run_secs(4.0);
    assert_eq!(
        name(&rig, "cam/1/close").as_deref(),
        Some("Guitarist 1 Close")
    );
    preview_and_cut(&mut rig, "cam/1/close");
    rig.run(2);
    playing.store(false, Ordering::SeqCst);
    let stale = rig.core.store(0).unwrap().config().alias_stale_passes;
    for _ in 0..((f64::from(stale) + 4.0) * FPS) as usize {
        rig.step();
        assert_eq!(
            name(&rig, "cam/1/close").as_deref(),
            Some("Guitarist 1 Close")
        );
    }
    let rec = rig.core.store(0).unwrap().record(SubjectId(1)).unwrap();
    let guitarist = QueryId("guitarist".into());
    assert!(rec.answers(&guitarist) && !rec.shot_queries().any(|q| *q == guitarist));
    let snap = rig.snapshot();
    assert_eq!(snap.switcher.program, Some(shot("cam/1/close")));
    assert!(snap.shot(&shot("cam/1/close")).unwrap().tally.program);
    // Off air, only the speaker's framing remains.
    preview_and_cut(&mut rig, "cam/wide");
    rig.apply(EngineCommand::SetPreview {
        shot: shot("cam/wide"),
    })
    .unwrap();
    rig.run(2);
    assert_eq!(name(&rig, "cam/1/close"), None);
    assert_eq!(
        name(&rig, "cam/1/medium").as_deref(),
        Some("Speaker 1 Medium")
    );
}
