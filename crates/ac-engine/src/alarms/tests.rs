//! The alarm model on fixture snapshots: a two-source studio's snapshot
//! built by a real core, with the program, sources, output and encoder
//! set by hand and the media time moved on observation by observation.
//! Every alarm is raised and cleared by its rule, with its hysteresis; an
//! acknowledged alarm stays listed until its cause clears; stale and
//! repeated snapshots change nothing. The program's rules are tested here,
//! the output's in `output`.

use std::path::PathBuf;

use ac_core::{Rect, SubjectId};
use ac_media::{SourceInfo, SourceState};
use ac_output::{EncoderChoice, EncoderStats, Progress, StreamHealth, StreamState};
use ac_sources::SourceConfig;

use super::*;
use crate::audio::AudioPacerStats;
use crate::catalog::ShotStatus;
use crate::config::{SourceEntry, StudioConfig};
use crate::core::EngineCore;
use crate::snapshot::{OutputState, OutputView, ShotView, Tally, TimingView};

mod output;

/// Program audio sample frames per second of the fixtures.
const RATE: f64 = 48_000.0;

/// A source of the fixture studio.
fn synthetic() -> SourceConfig {
    SourceConfig::Synthetic {
        width: 320,
        height: 180,
        fps: 30.0,
        subjects: 1,
    }
}

/// The wide of `source`.
fn wide(source: &str) -> ShotId {
    ShotId(format!("{source}/wide"))
}

/// The subject shot of the fixture studio.
fn closeup() -> ShotId {
    ShotId("stage/1/medium".into())
}

/// The snapshot of a studio with sources "stage" and "side", both
/// delivering, a subject shot of stage's subject 1 that tracks, nothing on
/// program and the output idle.
fn studio() -> EngineSnapshot {
    let mut config = StudioConfig::with_source("stage", synthetic());
    config.sources.push(SourceEntry {
        id: "side".into(),
        name: Some("Side".into()),
        input: synthetic(),
    });
    let infos = ["stage", "side"]
        .map(|id| SourceInfo {
            id: SourceId(id.into()),
            width: 320,
            height: 180,
            fps: 30.0,
            audio: None,
        })
        .to_vec();
    let core = EngineCore::new(&config, infos, &[false, false]).unwrap();
    let mut snap = core.snapshot(OutputView::default(), TimingView::default(), true);
    snap.shots.push(ShotView {
        id: closeup(),
        name: "Person 1 Medium".into(),
        source: SourceId("stage".into()),
        kind: ac_director::CandidateKind::Subject,
        subject: Some(SubjectId(1)),
        preset: Some("medium".into()),
        crop: Rect::new(0.25, 0.25, 0.5, 0.5),
        quality: 0.9,
        settled: 1.0,
        limited_by_zoom: false,
        same_picture_as: None,
        status: ShotStatus::Tracking,
        available: true,
        tally: Tally::default(),
        thumbnail: None,
    });
    for s in &mut snap.sources {
        s.state = SourceState::Running;
        s.frame_age = Some(Duration::ZERO);
        s.available = true;
    }
    snap.program_black_since = None;
    snap
}

/// The encoder statistics of a healthy session.
fn healthy_encoder() -> EncoderStats {
    EncoderStats {
        encoder: EncoderChoice::Libx264,
        frames_in: 0,
        frames_dropped: 0,
        frames_repeated: 0,
        frames_written: 0,
        audio_samples_in: 0,
        audio_samples_dropped: 0,
        audio_silence_samples: 0,
        audio_samples_written: 0,
        audio_connected: true,
        ffmpeg_alive: true,
        exit_code: None,
        last_error: None,
        progress: Progress::default(),
        stream: None,
    }
}

/// A stream's health in `state`.
fn stream(state: StreamState) -> StreamHealth {
    StreamHealth {
        state,
        stalled: false,
        outages: 1,
        reconnects: 0,
        bitrate_kbps: None,
    }
}

/// A stream's health in `state` with the ingest taking no packets.
fn stalled(state: StreamState) -> StreamHealth {
    StreamHealth {
        stalled: true,
        ..stream(state)
    }
}

/// The fixture studio observed over time: [`Show::at`] moves the media
/// time on, and while `audio_flows` the program audio pushed with it.
struct Show {
    /// The model under test.
    alarms: Alarms,
    /// The snapshot observed next.
    snap: EngineSnapshot,
    /// Source audio reaches the encoder at [`RATE`].
    audio_flows: bool,
}

impl Show {
    /// [`studio`], nothing observed yet.
    fn new() -> Self {
        Self {
            alarms: Alarms::new(),
            snap: studio(),
            audio_flows: true,
        }
    }

    /// The show with `shot` on program.
    fn program(mut self, shot: ShotId) -> Self {
        self.snap.switcher.program = Some(shot);
        self
    }

    /// The show recording and streaming, with program audio, a healthy
    /// encoder and a connected stream.
    fn on_air(mut self) -> Self {
        let o = &mut self.snap.output;
        o.state = OutputState::Running;
        o.record_path = Some(PathBuf::from("show.mp4"));
        o.rtmp_url = Some("rtmp://example.com/app/<redacted>".into());
        o.stats = Some(EncoderStats {
            stream: Some(stream(StreamState::Connected)),
            ..healthy_encoder()
        });
        o.audio = Some(AudioPacerStats::default());
        self
    }

    /// The encoder statistics of the session.
    fn encoder(&mut self) -> &mut EncoderStats {
        self.snap.output.stats.as_mut().expect("on air")
    }

    /// Observes the snapshot at media time `secs` (a tick per millisecond);
    /// returns what changed.
    fn at(&mut self, secs: f64) -> Vec<AlarmChange> {
        self.snap.media_time = Duration::from_secs_f64(secs);
        self.snap.tick = (secs * 1000.0).round() as u64;
        if self.audio_flows
            && let Some(a) = &mut self.snap.output.audio
        {
            a.pushed = (secs * RATE) as u64;
        }
        self.alarms.observe(&self.snap)
    }

    /// Observes every `step` seconds from `from` up to `to`, both included
    /// when `to` is a whole number of steps on.
    fn over(&mut self, from: f64, to: f64, step: f64) -> Vec<AlarmChange> {
        let mut changes = Vec::new();
        let n = ((to - from) / step + 1e-6).floor() as u32;
        for i in 0..=n {
            changes.extend(self.at(from + f64::from(i) * step));
        }
        changes
    }

    /// The causes of the active alarms, oldest first.
    fn causes(&self) -> Vec<AlarmCause> {
        self.alarms
            .active()
            .into_iter()
            .map(|a| a.cause.clone())
            .collect()
    }
}

/// The time `secs` as a duration.
fn s(secs: f64) -> Duration {
    Duration::from_secs_f64(secs)
}

#[test]
fn a_healthy_show_raises_nothing() {
    let mut show = Show::new().program(closeup()).on_air();
    assert_eq!(show.over(0.0, 30.0, 0.04), []);
    assert!(show.alarms.active().is_empty());
    // Nor does a studio with nothing on air and the output idle.
    let mut idle = Show::new();
    assert_eq!(idle.over(0.0, 10.0, 0.04), []);
}

#[test]
fn fixture_snapshots_raise_and_clear_each_alarm() {
    /// A way to bring a cause about on the show, and one to end it.
    type Edit = fn(&mut Show);
    let cases: [(AlarmCause, Edit, Edit); 10] = [
        (
            AlarmCause::ProgramBlack,
            |show| show.snap.program_black_since = Some(show.snap.media_time),
            |show| show.snap.program_black_since = None,
        ),
        (
            AlarmCause::ProgramSubjectLost(closeup()),
            |show| show.snap.shots.last_mut().unwrap().status = ShotStatus::Lost,
            |show| show.snap.shots.last_mut().unwrap().status = ShotStatus::Tracking,
        ),
        (
            AlarmCause::ProgramSourceStale(SourceId("stage".into())),
            |show| show.snap.sources[0].frame_age = Some(s(0.6)),
            |show| show.snap.sources[0].frame_age = Some(s(0.03)),
        ),
        (
            AlarmCause::EncoderDrops,
            |show| show.encoder().frames_dropped += 3,
            |_| {},
        ),
        (
            AlarmCause::OutputFailed,
            |show| show.snap.output.state = OutputState::Failed("ffmpeg exited".into()),
            |show| show.snap.output.state = OutputState::Running,
        ),
        (
            AlarmCause::StreamReconnecting,
            |show| show.encoder().stream = Some(stream(StreamState::Reconnecting { attempt: 1 })),
            |show| show.encoder().stream = Some(stream(StreamState::Connected)),
        ),
        (
            AlarmCause::StreamReconnecting,
            |show| show.encoder().stream = Some(stalled(StreamState::Connected)),
            |show| show.encoder().stream = Some(stream(StreamState::Connected)),
        ),
        (
            AlarmCause::StreamReconnecting,
            |show| show.encoder().stream = Some(stalled(StreamState::Connecting)),
            |show| show.encoder().stream = Some(stream(StreamState::Connected)),
        ),
        (
            AlarmCause::StreamDown,
            |show| show.encoder().stream = Some(stream(StreamState::Down)),
            |show| show.snap.output.state = OutputState::Finished,
        ),
        (
            AlarmCause::RecordingWithoutAudio,
            |show| show.audio_flows = false,
            |show| show.audio_flows = true,
        ),
    ];
    for (cause, start, end) in cases {
        let mut show = Show::new().program(closeup()).on_air();
        show.over(0.0, 1.0, 0.05);
        start(&mut show);
        let raised = show.over(1.0, 4.0, 0.05);
        assert!(
            matches!(&raised[..], [AlarmChange::Raised(a)] if a.cause == cause),
            "{cause}: {raised:?}"
        );
        let alarm = show.alarms.get(&cause).unwrap();
        assert!(
            !alarm.acknowledged && !alarm.message.is_empty(),
            "{alarm:?}"
        );
        assert!(alarm.raised_at >= s(1.0), "{cause}: {alarm:?}");
        end(&mut show);
        let cleared = show.over(4.0, 12.0, 0.05);
        assert!(
            matches!(&cleared[..], [AlarmChange::Cleared { alarm, .. }] if alarm.cause == cause),
            "{cause}: {cleared:?}"
        );
        assert!(show.alarms.active().is_empty(), "{cause}");
    }
}

#[test]
fn program_black_raises_after_two_seconds_on_air_and_clears_after_a_second_of_picture() {
    let mut show = Show::new().program(wide("stage")).on_air();
    show.at(0.0);
    show.snap.program_black_since = Some(s(0.5));
    assert_eq!(show.over(0.5, 2.45, 0.05), [], "a dip's black is shorter");
    let raised = show.at(2.5);
    let [AlarmChange::Raised(alarm)] = &raised[..] else {
        panic!("{raised:?}");
    };
    assert_eq!(alarm.cause, AlarmCause::ProgramBlack);
    assert_eq!(alarm.raised_at, s(2.5));
    assert_eq!(alarm.message, "program has been black for over 2 s");
    // A picture for less than a second keeps it; a second clears it.
    show.snap.program_black_since = None;
    assert_eq!(show.over(3.0, 3.95, 0.05), []);
    let cleared = show.at(4.0);
    assert!(matches!(&cleared[..], [AlarmChange::Cleared { at, .. }] if *at == s(4.0)));
    // Black while the output is idle is no alarm, however long.
    let mut idle = Show::new().program(wide("stage"));
    idle.snap.program_black_since = Some(Duration::ZERO);
    assert_eq!(idle.over(0.0, 20.0, 0.1), []);
}

#[test]
fn program_black_that_flickers_stays_raised_and_ends_with_the_output() {
    let mut show = Show::new().program(wide("stage")).on_air();
    show.snap.program_black_since = Some(Duration::ZERO);
    show.over(0.0, 2.0, 0.1);
    assert_eq!(show.causes(), [AlarmCause::ProgramBlack]);
    // Picture and short black runs by turns: never a clean second.
    for i in 0..40 {
        let t = 2.0 + f64::from(i) * 0.25;
        show.snap.program_black_since = (i % 2 == 0).then(|| s(t));
        assert_eq!(show.at(t), [], "at {t}");
    }
    // Stopping the output clears it at once.
    show.snap.output.state = OutputState::Stopping;
    let cleared = show.at(12.1);
    assert!(matches!(&cleared[..], [AlarmChange::Cleared { alarm, at }]
        if alarm.cause == AlarmCause::ProgramBlack && *at == s(12.1)));
}

#[test]
fn a_lost_subject_on_program_raises_after_half_a_second() {
    let lost = AlarmCause::ProgramSubjectLost(closeup());
    let mut show = Show::new().program(closeup());
    show.at(0.0);
    // A dropout shorter than the hold raises nothing.
    show.snap.shots.last_mut().unwrap().status = ShotStatus::Lost;
    assert_eq!(show.over(1.0, 1.45, 0.05), []);
    show.snap.shots.last_mut().unwrap().status = ShotStatus::Tracking;
    show.at(1.5);
    show.snap.shots.last_mut().unwrap().status = ShotStatus::Lost;
    assert_eq!(show.over(1.6, 2.05, 0.05), [], "the hold starts again");
    let raised = show.at(2.1);
    let [AlarmChange::Raised(alarm)] = &raised[..] else {
        panic!("{raised:?}");
    };
    assert_eq!(alarm.cause, lost);
    assert_eq!(
        alarm.message,
        "program shot Person 1 Medium: its subject is lost"
    );
    // A stalled source says nothing either way: the alarm stays.
    show.snap.shots.last_mut().unwrap().status = ShotStatus::Unavailable;
    assert_eq!(show.over(2.2, 5.0, 0.1), []);
    // Tracking again for a second clears it.
    show.snap.shots.last_mut().unwrap().status = ShotStatus::Tracking;
    assert_eq!(show.over(5.1, 6.0, 0.1), []);
    assert_eq!(show.at(6.1).len(), 1);
    // Cutting away from a lost subject clears its alarm at once.
    show.snap.shots.last_mut().unwrap().status = ShotStatus::Lost;
    show.over(6.2, 7.0, 0.1);
    assert_eq!(show.causes(), std::slice::from_ref(&lost));
    show.snap.switcher.program = Some(wide("stage"));
    let cleared = show.at(7.05);
    assert!(matches!(&cleared[..], [AlarmChange::Cleared { alarm, .. }] if alarm.cause == lost));
    // A wide is never on a lost subject.
    assert_eq!(show.over(7.1, 9.0, 0.1), []);
}

#[test]
fn a_program_source_older_than_half_a_second_raises_at_once() {
    let stage = AlarmCause::ProgramSourceStale(SourceId("stage".into()));
    let mut show = Show::new().program(wide("stage"));
    show.snap.sources[0].frame_age = Some(s(0.5));
    assert_eq!(show.at(0.0), [], "0.5 s is not older than 0.5 s");
    show.snap.sources[0].frame_age = Some(s(0.55));
    let raised = show.at(0.1);
    assert!(matches!(&raised[..], [AlarmChange::Raised(a)]
        if a.cause == stage && a.message == "program source stage sent no new frame for over 0.5 s"));
    // Offline: the message says so, with the reason.
    show.snap.sources[0].state = SourceState::Offline;
    show.snap.sources[0].error = Some("input lost".into());
    show.at(0.2);
    assert_eq!(
        show.alarms.get(&stage).unwrap().message,
        "program source stage is offline: input lost"
    );
    // Fresh frames clear it after a second; a 2 fps trickle does not.
    show.snap.sources[0].state = SourceState::Running;
    show.snap.sources[0].error = None;
    for i in 0..20 {
        show.snap.sources[0].frame_age = Some(s(0.15 * f64::from(i % 5)));
        assert_eq!(show.at(1.0 + f64::from(i) * 0.15), [], "tick {i}");
    }
    show.snap.sources[0].frame_age = Some(s(0.02));
    assert_eq!(show.over(4.0, 4.95, 0.05), []);
    assert_eq!(show.at(5.0).len(), 1);
    // A source that never delivered is stale too; taking the other
    // source's wide clears its alarm at once.
    show.snap.sources[0].frame_age = None;
    show.at(5.1);
    assert_eq!(
        show.alarms.get(&stage).unwrap().message,
        "program source stage has sent no picture yet"
    );
    show.snap.switcher.program = Some(wide("side"));
    let cleared = show.at(5.15);
    assert!(matches!(&cleared[..], [AlarmChange::Cleared { alarm, .. }] if alarm.cause == stage));
    assert!(show.alarms.active().is_empty());
}

#[test]
fn a_program_source_with_no_picture_yet_has_half_a_second_from_the_start() {
    let stage = AlarmCause::ProgramSourceStale(SourceId("stage".into()));
    let mut show = Show::new().program(wide("stage"));
    show.snap.sources[0].frame_age = None;
    assert_eq!(show.over(0.0, 0.5, 0.05), [], "no older than half a second");
    // Past it, the source is stale.
    let raised = show.at(0.55);
    assert!(matches!(&raised[..], [AlarmChange::Raised(a)]
        if a.cause == stage && a.raised_at == s(0.55)));
    // A first frame within the half second raises nothing.
    let mut prompt = Show::new().program(wide("stage"));
    prompt.snap.sources[0].frame_age = None;
    prompt.over(0.0, 0.45, 0.05);
    prompt.snap.sources[0].frame_age = Some(Duration::ZERO);
    assert_eq!(prompt.over(0.5, 5.0, 0.05), []);
}

#[test]
fn an_acknowledged_alarm_stays_listed_until_its_cause_clears() {
    let mut show = Show::new().program(wide("stage")).on_air();
    show.at(0.0);
    show.encoder().stream = Some(stream(StreamState::Reconnecting { attempt: 1 }));
    show.snap.sources[0].frame_age = Some(s(0.8));
    show.at(1.0);
    let stale = AlarmCause::ProgramSourceStale(SourceId("stage".into()));
    assert_eq!(
        show.causes(),
        [stale.clone(), AlarmCause::StreamReconnecting]
    );
    assert_eq!(show.alarms.unacknowledged(), 2);
    assert!(show.alarms.acknowledge(&AlarmCause::StreamReconnecting));
    assert!(
        !show.alarms.acknowledge(&AlarmCause::StreamDown),
        "not active"
    );
    assert_eq!(show.alarms.unacknowledged(), 1);
    // Acknowledged, it stays listed for as long as the cause lasts, with
    // its message kept up to date.
    for i in 1..=60 {
        show.encoder().stream = Some(stream(StreamState::Reconnecting { attempt: i }));
        assert_eq!(show.at(1.0 + f64::from(i)), []);
    }
    let alarm = show.alarms.get(&AlarmCause::StreamReconnecting).unwrap();
    assert!(alarm.acknowledged);
    assert_eq!(alarm.raised_at, s(1.0));
    assert!(alarm.message.contains("attempt 60"), "{}", alarm.message);
    assert_eq!(show.causes().len(), 2);
    // The cause clears: it goes, acknowledged or not.
    show.encoder().stream = Some(stream(StreamState::Connected));
    let cleared = show.over(62.0, 67.0, 0.5);
    assert!(matches!(&cleared[..], [AlarmChange::Cleared { alarm, .. }]
        if alarm.cause == AlarmCause::StreamReconnecting && alarm.acknowledged));
    assert_eq!(show.causes(), [stale]);
    // The cause back again is a new alarm, not acknowledged.
    show.encoder().stream = Some(stream(StreamState::Reconnecting { attempt: 1 }));
    show.at(68.0);
    let again = show.alarms.get(&AlarmCause::StreamReconnecting).unwrap();
    assert!(!again.acknowledged);
    assert_eq!(again.raised_at, s(68.0));
}

#[test]
fn alarms_are_listed_in_raise_order_one_per_cause() {
    let mut show = Show::new().program(closeup()).on_air();
    show.at(0.0);
    // The lost subject waits for its hold; the drop raises at once.
    show.snap.shots.last_mut().unwrap().status = ShotStatus::Lost;
    show.at(0.1);
    show.encoder().frames_dropped = 1;
    show.at(0.2);
    show.over(0.3, 1.0, 0.1);
    assert_eq!(
        show.causes(),
        [
            AlarmCause::EncoderDrops,
            AlarmCause::ProgramSubjectLost(closeup())
        ]
    );
    // Two raised by one observation keep the rules' order.
    let mut both = Show::new().program(wide("stage")).on_air();
    both.at(0.0);
    both.snap.sources[0].frame_age = Some(s(1.0));
    both.encoder().frames_dropped = 1;
    both.at(0.5);
    assert_eq!(
        both.causes(),
        [
            AlarmCause::ProgramSourceStale(SourceId("stage".into())),
            AlarmCause::EncoderDrops
        ]
    );
}

#[test]
fn stale_and_repeated_snapshots_change_nothing() {
    let mut show = Show::new().program(wide("stage")).on_air();
    show.at(0.0);
    let old = show.snap.clone();
    show.encoder().frames_dropped = 2;
    assert_eq!(show.at(1.0).len(), 1);
    let model = show.alarms.clone();
    // The same snapshot again, as a window drawing it twice does.
    assert_eq!(show.alarms.observe(&show.snap.clone()), []);
    assert_eq!(show.alarms, model);
    // An older one, as a reader that fell behind might hand in.
    assert_eq!(show.alarms.observe(&old), []);
    assert_eq!(show.alarms, model);
}

#[test]
fn a_stopped_engine_has_nothing_on_air_but_keeps_a_failed_output() {
    let mut show = Show::new().program(wide("stage")).on_air();
    show.snap.sources[0].frame_age = Some(s(2.0));
    show.at(0.0);
    show.snap.output.state = OutputState::Failed("killed".into());
    show.at(0.1);
    assert_eq!(show.causes().len(), 2);
    show.snap.running = false;
    let changes = show.at(0.2);
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(show.causes(), [AlarmCause::OutputFailed]);
}

#[test]
fn causes_name_their_kind() {
    assert_eq!(AlarmCause::ProgramBlack.kind(), "program_black");
    assert_eq!(
        AlarmCause::ProgramSubjectLost(closeup()).to_string(),
        "program subject lost"
    );
    assert_eq!(
        AlarmCause::RecordingWithoutAudio.to_string(),
        "recording without audio"
    );
}
