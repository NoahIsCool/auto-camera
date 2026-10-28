//! Window tests of the status bar's alarms: an alarm is listed, in red
//! with its own Acknowledge button, as soon as a snapshot shows its cause;
//! acknowledging dims it and sends nothing to the engine; it stays listed
//! until its cause clears, beside the notices, which keep their own path.
//! And a studio with `start_on_wide` opens with the wide on program.

use super::switching::{shot_of, studio_with};
use super::*;
use crate::ui::alarms::{acknowledge_label, alarm_line};
use ac_engine::{Alarm, AlarmCause};
use std::time::Duration;

/// The alarm the window shows for `cause` raised at `secs` with `message`.
fn alarm(cause: AlarmCause, secs: u64, message: &str, acknowledged: bool) -> Alarm {
    Alarm {
        cause,
        raised_at: Duration::from_secs(secs),
        message: message.into(),
        acknowledged,
    }
}

/// Moves the window's snapshot on to master-clock time `secs`, a tick per
/// frame of 30 fps, and draws until the window settles.
fn at(h: &mut Harness<'_, Window>, secs: u64) {
    let snap = &mut h.state_mut().snap;
    snap.media_time = Duration::from_secs(secs);
    snap.tick = secs * 30 + 1;
    h.run();
    // A new alarm row grows the status panel only on the next frame, which
    // egui does not ask for (the app repaints on its own timer): one more
    // lays the rows out where a click will find them.
    h.step();
}

#[test]
fn an_acknowledged_alarm_stays_listed_dimmed_until_its_cause_clears() {
    let mut snap = on_air();
    snap.output.state = OutputState::Failed("ffmpeg exited with status 1".into());
    let mut h = window(snap);
    h.run();
    let message = "output failed: ffmpeg exited with status 1";
    let raised = alarm(AlarmCause::OutputFailed, 0, message, false);
    assert_eq!(
        h.get_by_label(&alarm_line(&raised)).accesskit_node().role(),
        Role::Label
    );
    let label = acknowledge_label(&raised);
    let button = h.get_by_label(&label);
    assert_eq!(button.accesskit_node().role(), Role::Button);
    button.click();
    h.run();
    // Dimmed, without its button, and nothing went to the engine.
    let seen = alarm(AlarmCause::OutputFailed, 0, message, true);
    h.get_by_label(&alarm_line(&seen));
    assert!(h.query_by_label(&acknowledge_label(&raised)).is_none());
    assert_eq!(sent(&mut h), []);
    // Still listed while the output stays failed...
    at(&mut h, 30);
    h.get_by_label(&alarm_line(&seen));
    // ...and gone once it starts again.
    h.state_mut().snap.output.state = OutputState::Starting;
    at(&mut h, 31);
    assert!(h.query_by_label(&alarm_line(&seen)).is_none());
    assert!(h.query_by_label_contains("ALARM").is_none());
}

#[test]
fn each_alarm_has_its_own_acknowledge_button() {
    // The output failed (raised on the first frame), and the program's
    // source has sent nothing in the first second (raised a second on).
    let mut snap = snapshot();
    let wide = snap.shots[0].id.clone();
    snap.switcher.program = Some(wide);
    snap.output.state = OutputState::Failed("killed".into());
    let mut h = window(snap);
    at(&mut h, 1);
    let stale = alarm(
        AlarmCause::ProgramSourceStale(ac_core::SourceId("stage".into())),
        1,
        "program source stage has sent no picture yet",
        false,
    );
    let failed = alarm(AlarmCause::OutputFailed, 0, "output failed: killed", false);
    h.get_by_label(&alarm_line(&failed));
    h.get_by_label(&alarm_line(&stale));
    assert_eq!(h.get_all_by_label_contains("ALARM").count(), 2);
    let label = acknowledge_label(&stale);
    h.get_by_label(&label).click();
    h.run();
    h.get_by_label(&acknowledge_label(&failed));
    assert!(h.query_by_label(&acknowledge_label(&stale)).is_none());
    h.get_by_label(&alarm_line(&Alarm {
        acknowledged: true,
        ..stale
    }));
    assert_eq!(sent(&mut h), []);
}

#[test]
fn a_black_program_on_air_is_an_alarm_and_notices_keep_their_own_path() {
    // Streaming only, so the recording's audio rule stays out of it.
    let mut snap = on_air();
    snap.output.record_path = None;
    snap.program_black_since = Some(Duration::ZERO);
    let mut h = window(snap);
    at(&mut h, 1);
    assert!(h.query_by_label_contains("ALARM").is_none(), "not 2 s yet");
    at(&mut h, 3);
    let black = alarm(
        AlarmCause::ProgramBlack,
        3,
        "program has been black for over 2 s",
        false,
    );
    h.get_by_label(&alarm_line(&black));
    // A dropped take still raises its notice, beside the alarm.
    let wide = h.state().snap.shots[0].id.clone();
    h.state_mut().snap.switcher.dropped_takes = 1;
    h.state_mut().snap.switcher.last_dropped = Some(SwitchError::Unavailable(wide.clone()));
    at(&mut h, 4);
    h.get_by_label(&format!(
        "queued take: switcher: shot {wide:?} is not available"
    ));
    h.get_by_label(&alarm_line(&black));
    // The picture back for a second clears the alarm, not the notice.
    h.state_mut().snap.program_black_since = None;
    at(&mut h, 5);
    h.get_by_label(&alarm_line(&black));
    at(&mut h, 6);
    assert!(h.query_by_label_contains("ALARM").is_none());
}

#[test]
fn a_studio_that_starts_on_the_wide_opens_with_it_on_program() {
    let h = studio_with(|c| c.switcher.start_on_wide = true);
    let snap = h.state().snapshot();
    assert_eq!(snap.switcher.program, Some(shot_of(&h, 0)));
    let name = tile_name(0, &snap.shots[0]);
    assert!(name.contains("on program"), "{name}");
    h.get_by_label(&name);
    assert!(h.query_by_label_contains("ALARM").is_none());
}
