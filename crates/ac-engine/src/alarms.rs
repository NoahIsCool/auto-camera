//! Persistent alarms (spec M14, "visible errors"): what is wrong on air
//! now, read from the engine's snapshots.
//!
//! [`Alarms`] is a pure, deterministic model. Snapshots go in, in order,
//! each with its master-clock [`media_time`](EngineSnapshot::media_time);
//! out comes the list of active [`Alarm`]s, each with its cause, when it
//! was raised (media time), a sentence for the operator and whether the
//! operator has acknowledged it. It reads no clock and keeps no reference
//! to the engine: its reader owns it (the operator window keeps one), so
//! acknowledging is the reader's business and never reaches the engine.
//!
//! # Readings and hysteresis
//!
//! Each observation reads every cause it can say something about as
//! *bad* (the condition holds), *good* (it does not) or *unsure* (in
//! between: neither raises nor counts towards clearing). A cause raises
//! once it has read bad without a break for its raise hold, and clears
//! once it has read good without a break for its clear hold. A cause that
//! is not read at all is *gone*, for good: the output session is over, the
//! program moved to another shot or source, the engine stopped. A gone
//! cause clears at once, since what it watched has ended and cannot
//! flicker back.
//!
//! "Without a break" is between observations. Readers sample snapshots
//! (the window draws about one per frame, which may skip ticks or draw
//! one twice), so every rule is written over values that hold between
//! samples, never over a single tick a reader could miss: when the
//! program's run of black frames began, a source's frame age, the
//! encoder's drop counters and the program audio pushed, the last two
//! compared with the previous observation of the same output session.
//!
//! | Alarm | Raised when | Cleared when |
//! |---|---|---|
//! | program black | the output runs and the program has been black (nearly every pixel dark) for [`BLACK_AFTER`] | no black frame for [`CLEAR_AFTER`]; the output stops |
//! | program on a lost subject | the program shot's subject has been lost for [`LOST_AFTER`] | it has tracked again for [`CLEAR_AFTER`]; another shot is on program |
//! | program source stale | the program shot's source sent no new frame for over [`STALE_AFTER`] (or none in the first [`STALE_AFTER`] of media time, since the engine started) | its frames have been younger than that for [`CLEAR_AFTER`]; another source is on program |
//! | encoder drops | the encoder dropped frames or audio since the last observation | no new drop for [`QUIET_AFTER`]; the session ends |
//! | output failed | the output failed to start or ffmpeg exited with an error | the output starts again |
//! | stream reconnecting | the stream lost its connection and ffmpeg reopens it, or the ingest stopped taking packets (stalled: connected, it cannot keep up or went silent; connecting, it does not answer) | connected and taking packets for [`QUIET_AFTER`]; the stream is down or the session ends |
//! | stream down | ffmpeg gave the stream up for the session while still running (the recording goes on) | the session ends |
//! | recording without audio | the session records with no audio track, or no source audio was pushed for [`SILENT_AFTER`] | source audio for [`CLEAR_AFTER`], each piece within [`AUDIO_RECENT`] of the last; the session ends |
//!
//! The holds of the picture alarms are short, as a black or frozen program
//! is seen at once; those of the drop and stream alarms are longer, as
//! drops come in bursts and a connection that flaps should stay raised.
//! Program black is the only one gated on the output: before the show
//! starts a black program is normal. The program alarms follow whatever
//! is on program, output or not, since the operator rehearses on it.
//!
//! # Acknowledging
//!
//! [`Alarms::acknowledge`] marks an active alarm as seen: it stays listed
//! (the window dims it) until its cause clears, so acknowledging silences
//! an alarm without hiding a problem that is still there. A cause that
//! clears and comes back raises a new, unacknowledged alarm.
//!
//! # Invariants
//!
//! * At most one alarm per cause; a cause names the shot or source it is
//!   about, so the program cutting from one lost subject to another clears
//!   the first and raises the second.
//! * Alarms are listed in the order they were raised, ties in the order
//!   their causes first read bad (within one observation, the table's).
//! * Only a newer snapshot than the newest one observed is taken in: a
//!   higher tick, or the same tick's final snapshot (published with
//!   `running` false when the engine stops). The same snapshot again (a
//!   window draws one on several frames) and an older one change nothing,
//!   so the counter rules compare each snapshot once.

use std::fmt;
use std::time::Duration;

use ac_core::{ShotId, SourceId};

use crate::snapshot::EngineSnapshot;

mod rules;

use rules::{Reading, Session};

/// How long the program must have been black, while the output runs,
/// before the program-black alarm is raised: ffmpeg `blackdetect`'s
/// default shortest black, longer than the black of a dip of any allowed
/// length on a lit picture.
pub const BLACK_AFTER: Duration = Duration::from_secs(2);

/// How long the program shot's subject must have been lost before its
/// alarm is raised: longer than a tracker's brief dropout behind an
/// occluder.
pub const LOST_AFTER: Duration = Duration::from_millis(500);

/// A program source whose newest frame is older than this is stale.
pub const STALE_AFTER: Duration = Duration::from_millis(500);

/// How long no source audio may be pushed to a recording before its alarm
/// is raised.
pub const SILENT_AFTER: Duration = Duration::from_secs(2);

/// Source audio counts as flowing while each observation finds some pushed
/// within this of the last: several 10 ms blocks of jitter.
pub const AUDIO_RECENT: Duration = Duration::from_millis(250);

/// How long a picture or audio alarm's cause must read good before it
/// clears.
pub const CLEAR_AFTER: Duration = Duration::from_secs(1);

/// How long the encoder must go without a new drop, or the stream stay
/// connected and taking packets, before their alarms clear.
pub const QUIET_AFTER: Duration = Duration::from_secs(5);

/// What an alarm is about; see the module docs for each one's rules.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AlarmCause {
    /// The program has been black while the output runs.
    ProgramBlack,
    /// The shot on program frames a subject the tracker has lost.
    ProgramSubjectLost(ShotId),
    /// The source of the shot on program sends no new frames.
    ProgramSourceStale(SourceId),
    /// The encoder drops frames or audio: its queue was full.
    EncoderDrops,
    /// The output failed to start or ffmpeg exited with an error.
    OutputFailed,
    /// The stream is not getting through: it lost its connection and
    /// ffmpeg reopens it, or the ingest stopped taking packets (a stall,
    /// the only sign of a pulled cable until TCP reports it).
    StreamReconnecting,
    /// ffmpeg gave the stream up for the rest of the session.
    StreamDown,
    /// The recording gets no audio.
    RecordingWithoutAudio,
}

impl AlarmCause {
    /// The kind of cause in snake case, without the shot or source it is
    /// about: what [`Display`](fmt::Display) spells out in words.
    fn kind(&self) -> &'static str {
        match self {
            AlarmCause::ProgramBlack => "program_black",
            AlarmCause::ProgramSubjectLost(_) => "program_subject_lost",
            AlarmCause::ProgramSourceStale(_) => "program_source_stale",
            AlarmCause::EncoderDrops => "encoder_drops",
            AlarmCause::OutputFailed => "output_failed",
            AlarmCause::StreamReconnecting => "stream_reconnecting",
            AlarmCause::StreamDown => "stream_down",
            AlarmCause::RecordingWithoutAudio => "recording_without_audio",
        }
    }

    /// How long the cause must read bad before it raises. The black,
    /// stale and silence rules have their thresholds in the reading
    /// itself, measured by the engine or between observations, so they
    /// raise on the first bad reading.
    fn raise_after(&self) -> Duration {
        match self {
            AlarmCause::ProgramSubjectLost(_) => LOST_AFTER,
            _ => Duration::ZERO,
        }
    }

    /// How long the cause must read good before it clears.
    fn clear_after(&self) -> Duration {
        match self {
            AlarmCause::EncoderDrops | AlarmCause::StreamReconnecting => QUIET_AFTER,
            AlarmCause::OutputFailed | AlarmCause::StreamDown => Duration::ZERO,
            _ => CLEAR_AFTER,
        }
    }
}

impl fmt::Display for AlarmCause {
    /// The kind of cause in words: "program black", "stream down".
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.kind().replace('_', " "))
    }
}

/// One active alarm.
#[derive(Debug, Clone, PartialEq)]
pub struct Alarm {
    /// What it is about.
    pub cause: AlarmCause,
    /// Master-clock media time of the observation that raised it.
    pub raised_at: Duration,
    /// What is wrong, as a sentence for the operator, as the newest bad
    /// reading put it (a reconnection's attempt, the drop count).
    pub message: String,
    /// The operator has seen it; it stays listed until its cause clears.
    pub acknowledged: bool,
}

/// What one observation changed.
#[derive(Debug, Clone, PartialEq)]
pub enum AlarmChange {
    /// This alarm was raised.
    Raised(Alarm),
    /// This alarm, as it last stood, cleared at media time `at`.
    Cleared {
        /// The alarm.
        alarm: Alarm,
        /// When it cleared.
        at: Duration,
    },
}

/// The hysteresis of one cause that has read bad, or raised an alarm.
#[derive(Debug, Clone, PartialEq)]
struct Track {
    /// The cause.
    cause: AlarmCause,
    /// Start of the unbroken run of bad readings, if one is under way.
    bad_since: Option<Duration>,
    /// Start of the unbroken run of good readings, if one is under way.
    good_since: Option<Duration>,
    /// The raised alarm.
    alarm: Option<Alarm>,
    /// The cause was read by the observation under way.
    read: bool,
}

impl Track {
    /// A cause seen for the first time.
    fn new(cause: AlarmCause) -> Self {
        Self {
            cause,
            bad_since: None,
            good_since: None,
            alarm: None,
            read: false,
        }
    }

    /// Takes in `reading` (with `message`, for a bad one) at `now`; returns
    /// the alarm raised or cleared, if any (see the module docs).
    fn step(&mut self, now: Duration, reading: Reading, message: String) -> Option<AlarmChange> {
        self.read = true;
        match reading {
            Reading::Bad => {
                self.good_since = None;
                let since = *self.bad_since.get_or_insert(now);
                match &mut self.alarm {
                    Some(alarm) => {
                        alarm.message = message;
                        None
                    }
                    None if now.saturating_sub(since) >= self.cause.raise_after() => {
                        let alarm = Alarm {
                            cause: self.cause.clone(),
                            raised_at: now,
                            message,
                            acknowledged: false,
                        };
                        self.alarm = Some(alarm.clone());
                        Some(AlarmChange::Raised(alarm))
                    }
                    None => None,
                }
            }
            Reading::Good => {
                self.bad_since = None;
                let since = *self.good_since.get_or_insert(now);
                if now.saturating_sub(since) < self.cause.clear_after() {
                    return None;
                }
                let alarm = self.alarm.take()?;
                Some(AlarmChange::Cleared { alarm, at: now })
            }
            Reading::Unsure => {
                self.bad_since = None;
                self.good_since = None;
                None
            }
        }
    }

    /// True while the track has something to remember: a raised alarm or
    /// a run of bad readings that may raise one.
    fn live(&self) -> bool {
        self.alarm.is_some() || self.bad_since.is_some()
    }
}

/// The alarm model; see the module docs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Alarms {
    /// One per cause that has read bad or raised, in the order they first
    /// read bad.
    tracks: Vec<Track>,
    /// What the counter rules remember of the output session under way.
    session: Option<Session>,
    /// The newest snapshot observed: its tick, and whether the engine had
    /// stopped (the final snapshot repeats the last tick).
    last: Option<(u64, bool)>,
}

impl Alarms {
    /// A model that has seen nothing: no alarms.
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes in `snap`, the newest snapshot, and returns the alarms it
    /// raised and cleared. A snapshot already observed, or older than one,
    /// is dropped (see the module docs).
    pub fn observe(&mut self, snap: &EngineSnapshot) -> Vec<AlarmChange> {
        let key = (snap.tick, !snap.running);
        if self.last.is_some_and(|last| key <= last) {
            return Vec::new();
        }
        self.last = Some(key);
        let now = snap.media_time;
        for track in &mut self.tracks {
            track.read = false;
        }
        let mut changes = Vec::new();
        for r in rules::read(snap, &mut self.session) {
            let i = match self.tracks.iter().position(|t| t.cause == r.cause) {
                Some(i) => i,
                None if r.reading == Reading::Bad => {
                    self.tracks.push(Track::new(r.cause));
                    self.tracks.len() - 1
                }
                None => continue,
            };
            changes.extend(self.tracks[i].step(now, r.reading, r.message));
        }
        for track in self.tracks.iter_mut().filter(|t| !t.read) {
            if let Some(alarm) = track.alarm.take() {
                changes.push(AlarmChange::Cleared { alarm, at: now });
            }
        }
        self.tracks.retain(|t| t.read && t.live());
        changes
    }

    /// The active alarms, oldest first (see the module docs).
    pub fn active(&self) -> Vec<&Alarm> {
        let mut alarms: Vec<&Alarm> = self
            .tracks
            .iter()
            .filter_map(|t| t.alarm.as_ref())
            .collect();
        alarms.sort_by_key(|a| a.raised_at);
        alarms
    }

    /// The active alarm of `cause`, if one is raised.
    pub fn get(&self, cause: &AlarmCause) -> Option<&Alarm> {
        self.tracks
            .iter()
            .find(|t| &t.cause == cause)
            .and_then(|t| t.alarm.as_ref())
    }

    /// Marks the active alarm of `cause` as acknowledged; it stays listed
    /// until its cause clears. False when no such alarm is active (it
    /// cleared meanwhile, or never was).
    pub fn acknowledge(&mut self, cause: &AlarmCause) -> bool {
        let alarm = self
            .tracks
            .iter_mut()
            .find(|t| &t.cause == cause)
            .and_then(|t| t.alarm.as_mut());
        match alarm {
            Some(alarm) => {
                alarm.acknowledged = true;
                true
            }
            None => false,
        }
    }

    /// Active alarms not acknowledged yet.
    pub fn unacknowledged(&self) -> usize {
        self.active().iter().filter(|a| !a.acknowledged).count()
    }
}

#[cfg(test)]
mod tests;
