//! The readings of one snapshot: each cause it says something about, as
//! bad, good or unsure, with the sentence a bad reading raises (see the
//! parent module for the rules and their holds).
//!
//! The rules read only the snapshot, except for two counters that only
//! mean something as a change: the encoder's drops and the program audio
//! pushed. For those a [`Session`] remembers the previous observation of
//! the same output session. It starts on the first observation that finds
//! the output running, taking the counters as they are then (drops counted
//! before a reader started watching are not news), and ends on the first
//! that does not. A counter below the remembered one belongs to a new
//! encoder (the output restarted between two observations) and starts the
//! comparison over.

use std::time::Duration;

use ac_director::CandidateKind;
use ac_media::SourceState;
use ac_output::StreamState;

use super::{AUDIO_RECENT, AlarmCause, BLACK_AFTER, SILENT_AFTER, STALE_AFTER};
use crate::catalog::ShotStatus;
use crate::snapshot::{EngineSnapshot, OutputState};

/// What one observation says about one cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reading {
    /// The condition holds.
    Bad,
    /// It does not.
    Good,
    /// Neither: it neither raises nor counts towards clearing.
    Unsure,
}

/// One cause's reading, with the operator's sentence for it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Read {
    /// The cause.
    pub(super) cause: AlarmCause,
    /// What the snapshot says about it.
    pub(super) reading: Reading,
    /// What is wrong, for a bad reading.
    pub(super) message: String,
}

/// What the counter rules remember of the running output session; see
/// the module docs.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Session {
    /// Frames and audio samples the encoder had dropped, as last observed.
    drops: (u64, u64),
    /// Sample frames of source audio pushed, as last observed.
    audio_pushed: u64,
    /// Media time of the observation that last found more source audio
    /// pushed, or that started the session.
    audio_at: Duration,
}

impl Session {
    /// The session as `snap`, its first running observation, finds it.
    fn start(snap: &EngineSnapshot) -> Self {
        let o = &snap.output;
        Self {
            drops: o
                .stats
                .as_ref()
                .map_or((0, 0), |s| (s.frames_dropped, s.audio_samples_dropped)),
            audio_pushed: o.audio.map_or(0, |a| a.pushed),
            audio_at: snap.media_time,
        }
    }
}

/// Every reading of `snap`, in the order of the module table, keeping the
/// output session's counters in `session`.
pub(super) fn read(snap: &EngineSnapshot, session: &mut Option<Session>) -> Vec<Read> {
    let mut out = Vec::new();
    let live = snap.output.state == OutputState::Running;
    if !live {
        *session = None;
    }
    if snap.running {
        program(snap, live, &mut out);
    }
    if !live {
        if let OutputState::Failed(why) = &snap.output.state {
            push(
                &mut out,
                AlarmCause::OutputFailed,
                Reading::Bad,
                format!("output failed: {why}"),
            );
        }
        return out;
    }
    let s = session.get_or_insert_with(|| Session::start(snap));
    encoder(snap, s, &mut out);
    stream(snap, &mut out);
    audio(snap, s, &mut out);
    out
}

/// Appends one reading.
fn push(out: &mut Vec<Read>, cause: AlarmCause, reading: Reading, message: String) {
    out.push(Read {
        cause,
        reading,
        message,
    });
}

/// The program's readings: black (only while the output runs, `live`),
/// then the program shot's subject and source.
fn program(snap: &EngineSnapshot, live: bool, out: &mut Vec<Read>) {
    if live {
        let reading = match snap.program_black_since {
            Some(since) if snap.media_time.saturating_sub(since) >= BLACK_AFTER => Reading::Bad,
            Some(_) => Reading::Unsure,
            None => Reading::Good,
        };
        let message = format!(
            "program has been black for over {:.0} s",
            BLACK_AFTER.as_secs_f64()
        );
        push(out, AlarmCause::ProgramBlack, reading, message);
    }
    let Some(shot) = snap.switcher.program.as_ref().and_then(|id| snap.shot(id)) else {
        return;
    };
    if shot.kind == CandidateKind::Subject {
        let reading = match shot.status {
            ShotStatus::Lost => Reading::Bad,
            ShotStatus::Tracking => Reading::Good,
            // A stalled source says nothing about its subjects.
            ShotStatus::Unavailable | ShotStatus::Wide => Reading::Unsure,
        };
        push(
            out,
            AlarmCause::ProgramSubjectLost(shot.id.clone()),
            reading,
            format!("program shot {}: its subject is lost", shot.name),
        );
    }
    let Some(source) = snap.source(&shot.source) else {
        return;
    };
    let reading = match source.frame_age {
        Some(age) if age <= STALE_AFTER => Reading::Good,
        _ => Reading::Bad,
    };
    let name = &source.name;
    let mut message = match (source.state, source.frame_age) {
        (SourceState::Offline, _) => format!("program source {name} is offline"),
        (SourceState::Reconnecting, _) => format!("program source {name} is reconnecting"),
        (SourceState::Stopped, _) => format!("program source {name} has stopped"),
        (_, None) => format!("program source {name} has sent no picture yet"),
        (_, Some(_)) => format!(
            "program source {name} sent no new frame for over {:.1} s",
            STALE_AFTER.as_secs_f64()
        ),
    };
    if let Some(error) = &source.error {
        message.push_str(&format!(": {error}"));
    }
    push(
        out,
        AlarmCause::ProgramSourceStale(source.id.clone()),
        reading,
        message,
    );
}

/// The encoder's drops since the previous observation of the session.
fn encoder(snap: &EngineSnapshot, s: &mut Session, out: &mut Vec<Read>) {
    let Some(stats) = &snap.output.stats else {
        return;
    };
    let now = (stats.frames_dropped, stats.audio_samples_dropped);
    let (frames, samples) = s.drops;
    s.drops = now;
    let reading = if now.0 < frames || now.1 < samples {
        // A new encoder: compare from here.
        Reading::Good
    } else if now.0 > frames || now.1 > samples {
        Reading::Bad
    } else {
        Reading::Good
    };
    let mut message = format!("encoder dropped {} frames this session", now.0);
    if now.1 > 0 {
        message.push_str(&format!(" and {} audio samples", now.1));
    }
    push(out, AlarmCause::EncoderDrops, reading, message);
}

/// The stream's connection, for a session that streams.
fn stream(snap: &EngineSnapshot, out: &mut Vec<Read>) {
    let Some(stats) = &snap.output.stats else {
        return;
    };
    let Some(health) = stats.stream else {
        return;
    };
    let outages = match health.outages {
        1 => "1 outage".to_owned(),
        n => format!("{n} outages"),
    };
    let reconnecting = match health.state {
        StreamState::Reconnecting { attempt } => Some((
            Reading::Bad,
            format!("stream reconnecting (attempt {attempt}) · {outages}"),
        )),
        StreamState::Connected => Some((Reading::Good, String::new())),
        StreamState::Connecting => Some((Reading::Unsure, String::new())),
        StreamState::Down => None,
    };
    if let Some((reading, message)) = reconnecting {
        push(out, AlarmCause::StreamReconnecting, reading, message);
    }
    // Once ffmpeg has exited the stream is down by definition, and the
    // output's own state says why.
    if health.state == StreamState::Down && stats.ffmpeg_alive {
        let message = if snap.output.record_path.is_some() {
            "stream down for this session; the recording goes on"
        } else {
            "stream down for this session"
        };
        push(out, AlarmCause::StreamDown, Reading::Bad, message.into());
    }
}

/// Program audio, for a session that records: whether it has an audio
/// track, and how long since source audio was last pushed.
fn audio(snap: &EngineSnapshot, s: &mut Session, out: &mut Vec<Read>) {
    if snap.output.record_path.is_none() {
        return;
    }
    let now = snap.media_time;
    let Some(pacer) = snap.output.audio else {
        let message = "recording has no audio track (no output.audio_source, or it has no audio)";
        push(
            out,
            AlarmCause::RecordingWithoutAudio,
            Reading::Bad,
            message.into(),
        );
        return;
    };
    if pacer.pushed != s.audio_pushed {
        // More audio, or a new session's pacer starting over.
        s.audio_pushed = pacer.pushed;
        s.audio_at = now;
    }
    let quiet = now.saturating_sub(s.audio_at);
    let reading = if quiet >= SILENT_AFTER {
        Reading::Bad
    } else if quiet <= AUDIO_RECENT {
        Reading::Good
    } else {
        Reading::Unsure
    };
    push(
        out,
        AlarmCause::RecordingWithoutAudio,
        reading,
        format!(
            "no source audio reached the recording for over {:.0} s",
            SILENT_AFTER.as_secs_f64()
        ),
    );
}
