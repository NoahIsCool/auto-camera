//! The output's alarms on the fixture show of the parent module: encoder
//! drops counted per session, a failed output, the stream reconnecting,
//! stalled or down, a recording without audio, and program audio dropped
//! for its format.

use super::*;
use crate::program_audio::{AudioInputView, unheard};

#[test]
fn encoder_drops_raise_when_they_grow_and_clear_after_five_quiet_seconds() {
    let mut show = Show::new().program(wide("stage")).on_air();
    // Drops counted before the model watched are not news.
    show.encoder().frames_dropped = 7;
    assert_eq!(show.at(0.0), []);
    assert_eq!(show.at(1.0), []);
    show.encoder().audio_samples_dropped = 960;
    let raised = show.at(1.1);
    assert!(matches!(&raised[..], [AlarmChange::Raised(a)]
        if a.message == "encoder dropped 7 frames this session and 960 audio samples"));
    // A new drop restarts the quiet time.
    show.over(1.2, 4.0, 0.1);
    show.encoder().frames_dropped = 9;
    show.at(4.1);
    assert_eq!(
        show.alarms.get(&AlarmCause::EncoderDrops).unwrap().message,
        "encoder dropped 9 frames this session and 960 audio samples"
    );
    assert_eq!(show.over(4.2, 9.0, 0.1), []);
    assert_eq!(show.at(9.2).len(), 1, "cleared five seconds after the drop");
    // A restarted encoder counts from zero again: lower counts are not drops.
    show.encoder().frames_dropped = 0;
    show.encoder().audio_samples_dropped = 0;
    assert_eq!(show.over(9.3, 12.0, 0.1), []);
    // The session ending clears a raised alarm at once.
    show.encoder().frames_dropped = 1;
    assert_eq!(show.at(12.1).len(), 1);
    show.snap.output.state = OutputState::Finished;
    assert_eq!(show.at(12.2).len(), 1);
    assert!(show.alarms.active().is_empty());
}

#[test]
fn a_failed_output_stays_until_the_output_starts_again() {
    let mut show = Show::new().on_air();
    show.at(0.0);
    show.snap.output.state = OutputState::Failed("ffmpeg exited with status 1".into());
    let raised = show.at(0.5);
    assert!(matches!(&raised[..], [AlarmChange::Raised(a)]
        if a.message == "output failed: ffmpeg exited with status 1"));
    assert_eq!(show.over(1.0, 120.0, 1.0), []);
    show.snap.output.state = OutputState::Starting;
    assert_eq!(show.at(121.0).len(), 1);
    assert!(show.alarms.active().is_empty());
}

#[test]
fn a_stream_that_reconnects_then_goes_down() {
    let mut show = Show::new().program(wide("stage")).on_air();
    show.at(0.0);
    show.encoder().stream = Some(stream(StreamState::Reconnecting { attempt: 1 }));
    assert_eq!(show.at(0.1).len(), 1);
    show.encoder().stream = Some(stream(StreamState::Reconnecting { attempt: 2 }));
    show.at(2.0);
    assert_eq!(
        show.alarms
            .get(&AlarmCause::StreamReconnecting)
            .unwrap()
            .message,
        "stream reconnecting (attempt 2) · 1 outage"
    );
    // Connected for less than five seconds, then lost again: still raised.
    show.encoder().stream = Some(stream(StreamState::Connected));
    assert_eq!(show.over(2.1, 6.9, 0.1), []);
    show.encoder().stream = Some(stream(StreamState::Reconnecting { attempt: 1 }));
    assert_eq!(show.at(7.0), []);
    // Given up while ffmpeg runs on: reconnecting clears, down raises.
    show.encoder().stream = Some(stream(StreamState::Down));
    let changes = show.at(8.0);
    assert!(matches!(&changes[..], [
        AlarmChange::Raised(down),
        AlarmChange::Cleared { alarm, .. },
    ] if down.cause == AlarmCause::StreamDown
        && down.message == "stream down for this session; the recording goes on"
        && alarm.cause == AlarmCause::StreamReconnecting));
    assert_eq!(show.over(8.1, 30.0, 0.5), [], "down for the session");
    // Once ffmpeg has exited the output's state tells, not the stream.
    show.encoder().ffmpeg_alive = false;
    assert_eq!(show.at(30.1).len(), 1);
    // A stream that is only connecting is neither.
    let mut fresh = Show::new().on_air();
    fresh.encoder().stream = Some(stream(StreamState::Connecting));
    assert_eq!(fresh.over(0.0, 10.0, 0.1), []);
}

#[test]
fn a_stalled_stream_raises_the_stream_alarm_until_the_ingest_takes_packets_again() {
    let reconnecting = AlarmCause::StreamReconnecting;
    // The ingest stopped taking packets while connected (a pulled cable
    // TCP has not reported yet, an ingest that cannot keep up): raised at
    // once, and kept for as long as the stall lasts.
    let mut show = Show::new().program(wide("stage")).on_air();
    show.at(0.0);
    show.encoder().stream = Some(StreamHealth {
        outages: 0,
        ..stalled(StreamState::Connected)
    });
    let raised = show.at(0.1);
    assert!(
        matches!(&raised[..], [AlarmChange::Raised(a)]
            if a.cause == reconnecting && a.message == "stream stalled, dropping packets"),
        "{raised:?}"
    );
    assert_eq!(show.over(0.2, 60.0, 0.2), [], "stalled for a minute");
    // TCP gives up at last and ffmpeg reconnects: the same alarm goes on.
    show.encoder().stream = Some(stream(StreamState::Reconnecting { attempt: 1 }));
    assert_eq!(show.at(60.1), []);
    assert_eq!(
        show.alarms.get(&reconnecting).unwrap().message,
        "stream reconnecting (attempt 1) · 1 outage"
    );
    // Connected again but not yet taking packets is still a stall, for
    // however long: it never counts towards clearing.
    show.encoder().stream = Some(stalled(StreamState::Connected));
    assert_eq!(show.over(60.2, 70.0, 0.2), []);
    assert_eq!(
        show.alarms.get(&reconnecting).unwrap().message,
        "stream stalled, dropping packets · 1 outage"
    );
    // Taking packets for five seconds clears it.
    show.encoder().stream = Some(stream(StreamState::Connected));
    assert_eq!(show.over(70.2, 75.0, 0.2), []);
    let cleared = show.at(75.2);
    assert!(
        matches!(&cleared[..], [AlarmChange::Cleared { alarm, .. }] if alarm.cause == reconnecting),
        "{cleared:?}"
    );
    // An ingest that never answers the first connection: raised at once.
    let mut fresh = Show::new().on_air();
    fresh.encoder().stream = Some(stream(StreamState::Connecting));
    assert_eq!(fresh.over(0.0, 3.0, 0.1), []);
    fresh.encoder().stream = Some(StreamHealth {
        outages: 0,
        ..stalled(StreamState::Connecting)
    });
    let raised = fresh.at(3.1);
    assert!(
        matches!(&raised[..], [AlarmChange::Raised(a)]
            if a.cause == reconnecting && a.message == "stream connecting: the ingest does not answer"),
        "{raised:?}"
    );
    assert_eq!(fresh.over(3.2, 30.0, 0.2), []);
}

#[test]
fn a_recording_without_an_audio_track_or_without_source_audio() {
    // No audio track: raised at once, for the whole session.
    let mut show = Show::new().on_air();
    show.snap.output.audio = None;
    let raised = show.at(0.0);
    assert!(matches!(&raised[..], [AlarmChange::Raised(a)]
        if a.cause == AlarmCause::RecordingWithoutAudio && a.message.starts_with("recording has no audio track")));
    assert_eq!(show.over(0.1, 30.0, 0.5), []);
    // Streaming only is not a recording.
    let mut stream_only = Show::new().on_air();
    stream_only.snap.output.record_path = None;
    stream_only.snap.output.audio = None;
    assert_eq!(stream_only.over(0.0, 10.0, 0.1), []);
    // A track whose source stops: raised after two seconds of silence.
    let mut show = Show::new().on_air();
    show.over(0.0, 1.0, 0.05);
    show.audio_flows = false;
    assert_eq!(show.over(1.05, 2.95, 0.05), []);
    assert_eq!(show.at(3.0).len(), 1);
    // One block every second and a half never clears it...
    let mut pushed = show.snap.output.audio.unwrap().pushed;
    for i in 0..10 {
        let t = 3.0 + f64::from(i) * 0.25;
        if i % 6 == 0 {
            pushed += 480;
            show.snap.output.audio.as_mut().unwrap().pushed = pushed;
        }
        assert_eq!(show.at(t), [], "at {t}");
    }
    // ...audio that flows for a second does.
    show.audio_flows = true;
    assert_eq!(show.over(6.0, 6.95, 0.05), []);
    assert_eq!(show.at(7.0).len(), 1);
}

#[test]
fn program_audio_dropped_for_its_format_raises_for_a_stream_too() {
    // A session that only streams, its program audio at 48 kHz.
    let mut show = Show::new().program(wide("stage")).on_air();
    show.snap.output.record_path = None;
    show.snap.output.audio_format = Some(ac_output::AudioFormat {
        sample_rate: 48_000,
        channels: 2,
    });
    // Blocks dropped before the model watched are not news.
    show.snap.output.audio.as_mut().unwrap().mismatched_blocks = 4;
    assert_eq!(show.over(0.0, 1.0, 0.1), []);
    // The program's board, back at 96 kHz, says why it is not heard.
    let text = unheard("Soundboard", 96_000, 48_000);
    show.snap.audio_inputs = vec![AudioInputView {
        id: SourceId("board".into()),
        name: "Soundboard".into(),
        state: SourceState::Running,
        error: Some(text.clone()),
        format: Some((96_000, 2)),
        pick: None,
        program: true,
        blocks: 0,
        dropped: 0,
        unusable: 0,
        unheard: true,
        correction_ppm: 0.0,
        drift_resets: 0,
        drift_steps: 0,
    }];
    show.audio_mismatched = true;
    let raised = show.at(1.1);
    assert!(matches!(&raised[..], [AlarmChange::Raised(a)]
        if a.cause == AlarmCause::ProgramAudioUnheard && a.message == text));
    assert_eq!(
        text,
        "Soundboard runs at 96000 Hz; the output was started at 48000 Hz: not heard until \
         the output is restarted"
    );
    assert_eq!(show.over(1.2, 5.0, 0.1), [], "stays raised while it drops");
    // Any other origin (a source's audio at another rate): the session's
    // format is named.
    show.snap.audio_inputs.clear();
    show.at(5.1);
    assert_eq!(
        show.alarms
            .get(&AlarmCause::ProgramAudioUnheard)
            .unwrap()
            .message,
        "program audio is not in the format the output was started with (48000 Hz, 2 \
         channels): not heard until the output is restarted"
    );
    // Nothing dropped for a second clears it; a new session's pacer
    // counting from zero again is no drop.
    show.audio_mismatched = false;
    assert_eq!(show.over(5.2, 6.0, 0.1), []);
    assert_eq!(show.at(6.2).len(), 1);
    show.snap.output.audio.as_mut().unwrap().mismatched_blocks = 0;
    assert_eq!(show.over(6.3, 8.0, 0.1), []);
}
