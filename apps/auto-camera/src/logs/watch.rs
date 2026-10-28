//! The snapshot watcher: logs what changed in the studio, and keeps the
//! crash reporter's summary current.
//!
//! # Theory
//!
//! The core crates log nothing; they publish state as data, once per tick,
//! in the engine's snapshot. The log learns about the studio by watching
//! that snapshot from outside: a thread (`ac-log-watch`) reads the newest
//! one a few times a second ([`POLL`]), compares it with what it saw last
//! ([`Watcher::observe`]) and logs each change worth a line: the engine
//! starting and stopping (with the reason when it died), a source changing
//! state, a detector becoming ready or failing, program changing, the
//! director's mode, the output's lifecycle and the stream's connection.
//! It also renders the snapshot's summary (`summary` module) for the crash
//! reporter, so a crash report says what the studio was doing.
//!
//! Detector errors and timeouts can come every pass, so they are
//! throttled: a note is logged when the error reads differently from the
//! last one noted (numbers aside, so "after 1.2 s" and "after 1.3 s" read
//! the same) or [`QUIET`] of media time after the last note, and says how
//! many came since. What is still unnoted when the engine stops is noted
//! then.
//!
//! Polling misses changes that last less than a poll (a state that comes
//! and goes within 250 ms); the counters, which only grow, are never
//! missed.
//!
//! # Invariants
//!
//! * [`Watcher::observe`] is pure: its notes depend on the snapshots seen
//!   so far only, and time is the snapshot's media time.
//! * The watcher never sends the engine anything; it only reads
//!   snapshots.
//! * Dropping the [`Watch`] observes the newest snapshot once more (the
//!   final one, after a shutdown) and joins the thread; the thread also
//!   ends by itself once the engine has stopped.

use std::collections::HashMap;
use std::mem::discriminant;
use std::sync::Arc;
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use ac_core::{ShotId, SourceId};
use ac_director::DirectorMode;
use ac_engine::{EngineHandle, EngineSnapshot, OutputState, OutputView, SourceView};
use ac_media::SourceState;
use ac_output::StreamState;
use tracing::Level;

use super::{crash, summary};

/// How often the thread reads the newest snapshot.
pub const POLL: Duration = Duration::from_millis(250);

/// Media time after a throttled note before the same kind of note is
/// logged again.
pub const QUIET: Duration = Duration::from_secs(60);

/// The target of the watcher's events.
const TARGET: &str = "auto_camera::engine";

/// One line the watcher logs: its level and text.
pub type Note = (Level, String);

/// The watcher thread; dropping it stops the thread (see the module docs).
#[derive(Debug)]
pub struct Watch {
    /// Dropped to stop the thread.
    stop: Option<Sender<()>>,
    /// The thread, until joined.
    thread: Option<JoinHandle<()>>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Watches the engine behind `handle`, logging changes and storing the
/// summary of its newest snapshot with the crash reporter. When the thread
/// cannot be started the studio runs unwatched, which the log says.
pub fn watch(handle: EngineHandle) -> Watch {
    spawn(move || handle.snapshot(), log, crash::publish_summary)
}

/// Starts the thread on `snapshot`, giving every note to `emit` and every
/// new snapshot's summary to `publish`.
fn spawn(
    mut snapshot: impl FnMut() -> Arc<EngineSnapshot> + Send + 'static,
    mut emit: impl FnMut(Level, &str) + Send + 'static,
    mut publish: impl FnMut(&str) + Send + 'static,
) -> Watch {
    let (stop, stopped) = channel::<()>();
    let thread = std::thread::Builder::new()
        .name("ac-log-watch".into())
        .spawn(move || {
            let mut watcher = Watcher::default();
            // Observes the newest snapshot unless it is `last`, the one
            // seen before; true while the engine runs.
            let mut look = |last: &mut Option<Arc<EngineSnapshot>>| {
                let snap = snapshot();
                if last.as_ref().is_none_or(|l| !Arc::ptr_eq(l, &snap)) {
                    for (level, text) in watcher.observe(&snap) {
                        emit(level, &text);
                    }
                    publish(&summary::render(&snap));
                }
                let running = snap.running;
                *last = Some(snap);
                running
            };
            let mut last = None;
            while look(&mut last) {
                if !matches!(stopped.recv_timeout(POLL), Err(RecvTimeoutError::Timeout)) {
                    // Asked to stop: look once more, then end.
                    look(&mut last);
                    break;
                }
            }
        });
    let thread = match thread {
        Ok(t) => Some(t),
        Err(e) => {
            tracing::warn!(target: TARGET, "the studio is not watched for the log: {e}");
            None
        }
    };
    Watch {
        stop: Some(stop),
        thread,
    }
}

/// Logs `text` at `level` under the watcher's target.
fn log(level: Level, text: &str) {
    match level {
        Level::ERROR => tracing::error!(target: TARGET, "{text}"),
        Level::WARN => tracing::warn!(target: TARGET, "{text}"),
        Level::INFO => tracing::info!(target: TARGET, "{text}"),
        Level::DEBUG => tracing::debug!(target: TARGET, "{text}"),
        _ => tracing::trace!(target: TARGET, "{text}"),
    }
}

/// A counter whose growth is noted at most once per [`QUIET`] unless its
/// text reads differently; see the module docs.
#[derive(Debug, Default)]
struct Throttle {
    /// The count when last seen.
    seen: u64,
    /// The count when last noted.
    noted: u64,
    /// The shape of the text last noted.
    shape: Option<String>,
    /// Media time of the last note.
    at: Option<Duration>,
}

impl Throttle {
    /// The number to note now, if any, given the counter's `total`, the
    /// text of its newest event and the media time `now`.
    fn update(&mut self, total: u64, text: Option<&str>, now: Duration) -> Option<u64> {
        if total < self.seen {
            // A new count (cannot happen within one engine): start over.
            *self = Self::default();
        }
        if total == self.seen {
            return None;
        }
        self.seen = total;
        let shape = text.map(shape);
        let quiet = self.at.is_none_or(|at| now.saturating_sub(at) >= QUIET);
        if shape == self.shape && !quiet {
            return None;
        }
        let new = total - self.noted;
        self.noted = total;
        self.shape = shape;
        self.at = Some(now);
        Some(new)
    }

    /// The number seen but not yet noted, which this marks noted.
    fn flush(&mut self) -> Option<u64> {
        let new = self.seen - self.noted;
        self.noted = self.seen;
        (new > 0).then_some(new)
    }
}

/// `text` with every run of digits replaced by `#`, so messages that
/// differ only in their numbers read the same.
fn shape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_number = false;
    for c in text.chars() {
        if c.is_ascii_digit() {
            if !in_number {
                out.push('#');
            }
            in_number = true;
        } else {
            out.push(c);
            in_number = false;
        }
    }
    out
}

/// What the watcher remembers of one source.
#[derive(Debug)]
struct SourceSeen {
    /// Its state.
    state: SourceState,
    /// Its detector was loading.
    preparing: bool,
    /// Its detector errors.
    errors: Throttle,
    /// Its timed-out passes.
    timeouts: Throttle,
}

/// What the watcher remembers between snapshots; see the module docs.
#[derive(Debug, Default)]
pub struct Watcher {
    /// A snapshot was seen.
    started: bool,
    /// The engine had stopped.
    stopped: bool,
    /// The program shot.
    program: Option<ShotId>,
    /// The director's mode.
    mode: Option<DirectorMode>,
    /// The sources.
    sources: HashMap<SourceId, SourceSeen>,
    /// The output's state.
    output: OutputState,
    /// The stream's state.
    stream: Option<StreamState>,
}

impl Watcher {
    /// The notes `snap` gives, compared with the snapshots seen before.
    pub fn observe(&mut self, snap: &EngineSnapshot) -> Vec<Note> {
        let mut notes = Vec::new();
        if self.stopped {
            return notes;
        }
        if !self.started {
            self.started = true;
            self.mode = Some(snap.director.mode);
            notes.push(info(format!(
                "engine running: {} sources, {} queries, director {}{}",
                snap.sources.len(),
                snap.queries.len(),
                lower(&snap.director.mode),
                if snap.sidecar.is_some() {
                    ", detector sidecar"
                } else {
                    ""
                }
            )));
        }
        for source in &snap.sources {
            self.source(source, snap.media_time, &mut notes);
        }
        if self.mode != Some(snap.director.mode) {
            self.mode = Some(snap.director.mode);
            notes.push(info(format!("director: {}", lower(&snap.director.mode))));
        }
        if self.program != snap.switcher.program {
            self.program.clone_from(&snap.switcher.program);
            notes.push(info(match &snap.switcher.program {
                Some(id) => match snap.shot(id) {
                    Some(shot) => format!("program: {} ({})", shot.name, id.0),
                    None => format!("program: {}", id.0),
                },
                None => "program: nothing".to_owned(),
            }));
        }
        self.output(&snap.output, &mut notes);
        if !snap.running {
            self.stop(snap, &mut notes);
        }
        notes
    }

    /// Notes about `s`, seen at media time `now`.
    fn source(&mut self, s: &SourceView, now: Duration, notes: &mut Vec<Note>) {
        let id = &s.id.0;
        let seen = self.sources.entry(s.id.clone()).or_insert_with(|| {
            notes.push(info(format!(
                "source {id} ({}): {}x{} at {} fps, {}{}",
                s.name,
                s.width,
                s.height,
                s.nominal_fps,
                lower(&s.state),
                if s.discovery_enabled {
                    if s.discovery.preparing {
                        ", detector loading"
                    } else {
                        ", detector ready"
                    }
                } else {
                    ""
                }
            )));
            SourceSeen {
                state: s.state,
                preparing: s.discovery.preparing,
                errors: Throttle::default(),
                timeouts: Throttle::default(),
            }
        });
        if seen.state != s.state {
            let level = if s.state == SourceState::Running {
                Level::INFO
            } else {
                Level::WARN
            };
            notes.push((
                level,
                format!("source {id}: {} -> {}", lower(&seen.state), lower(&s.state)),
            ));
            seen.state = s.state;
        }
        let d = &s.discovery;
        if seen.preparing && !d.preparing {
            notes.push(info(match d.ready_after {
                Some(t) => format!(
                    "source {id}: detector ready at {:.1} s of media time",
                    t.as_secs_f64()
                ),
                None => format!("source {id}: detector ready"),
            }));
        }
        seen.preparing = d.preparing;
        if let Some(n) = seen.errors.update(d.errors, d.last_error.as_deref(), now) {
            notes.push(errors_note(id, n, d.last_error.as_deref()));
        }
        if let Some(n) = seen.timeouts.update(d.timeouts, None, now) {
            notes.push(timeouts_note(id, n));
        }
    }

    /// Notes about the output and its stream.
    fn output(&mut self, o: &OutputView, notes: &mut Vec<Note>) {
        if self.output != o.state {
            self.output = o.state.clone();
            notes.push(match &o.state {
                OutputState::Failed(why) => (Level::ERROR, format!("output failed: {why}")),
                OutputState::Running => {
                    let mut text = "output running".to_owned();
                    if let Some(path) = &o.record_path {
                        text.push_str(&format!(", recording {}", path.display()));
                    }
                    if let Some(url) = &o.rtmp_url {
                        text.push_str(&format!(", streaming to {url}"));
                    }
                    info(text)
                }
                OutputState::Finished => info(match &o.stats {
                    Some(st) => format!(
                        "output finished: {} frames written, {} dropped",
                        st.frames_written, st.frames_dropped
                    ),
                    None => "output finished".to_owned(),
                }),
                state => info(format!("output {}", lower(state))),
            });
        }
        let stream = o
            .stats
            .as_ref()
            .and_then(|s| s.stream.as_ref())
            .map(|h| h.state);
        let changed = match (&self.stream, &stream) {
            (Some(a), Some(b)) => discriminant(a) != discriminant(b),
            (None, Some(_)) => true,
            _ => false,
        };
        if changed && let Some(state) = stream {
            // The stream goes down at every stop; only an outage warns.
            let outage = match state {
                StreamState::Reconnecting { .. } => true,
                StreamState::Down => o.state == OutputState::Running,
                _ => false,
            };
            let level = if outage { Level::WARN } else { Level::INFO };
            notes.push((level, format!("stream {state}")));
        }
        self.stream = stream;
    }

    /// Notes when the engine has stopped: why, what was not yet noted, and
    /// the sidecar's final counts.
    fn stop(&mut self, snap: &EngineSnapshot, notes: &mut Vec<Note>) {
        self.stopped = true;
        for source in &snap.sources {
            let Some(seen) = self.sources.get_mut(&source.id) else {
                continue;
            };
            let id = &source.id.0;
            if let Some(n) = seen.errors.flush() {
                notes.push(errors_note(id, n, source.discovery.last_error.as_deref()));
            }
            if let Some(n) = seen.timeouts.flush() {
                notes.push(timeouts_note(id, n));
            }
        }
        if let Some(s) = &snap.sidecar {
            notes.push(info(format!(
                "sidecar: {} starts ({} restarts), {} failures, {} timeouts over the run",
                s.stats.spawns,
                s.stats.restarts(),
                s.stats.failures,
                s.stats.timeouts
            )));
        }
        notes.push(match &snap.fatal {
            Some(fatal) => (Level::ERROR, format!("engine stopped: {fatal}")),
            None => info(format!(
                "engine stopped after {} ticks, {:.1} s of media time",
                snap.tick,
                snap.media_time.as_secs_f64()
            )),
        });
    }
}

/// An `INFO` note.
fn info(text: String) -> Note {
    (Level::INFO, text)
}

/// The `Debug` text of `value`, lower case: `Running` reads `running`.
fn lower(value: &impl std::fmt::Debug) -> String {
    format!("{value:?}").to_lowercase()
}

/// The note of `n` detector errors on source `id`, the newest `error`.
fn errors_note(id: &str, n: u64, error: Option<&str>) -> Note {
    let error = error.unwrap_or("(no message)");
    let text = if n == 1 {
        format!("source {id}: detector error: {error}")
    } else {
        format!("source {id}: {n} detector errors, the newest: {error}")
    };
    (Level::WARN, text)
}

/// The note of `n` timed-out passes on source `id`.
fn timeouts_note(id: &str, n: u64) -> Note {
    let text = if n == 1 {
        format!("source {id}: a detector pass timed out")
    } else {
        format!("source {id}: {n} detector passes timed out")
    };
    (Level::WARN, text)
}

#[cfg(test)]
mod tests;
