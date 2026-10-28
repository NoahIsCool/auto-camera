//! The Auto Camera engine: everything between the sources and the program
//! output, wired with message passing.
//!
//! # Pipeline
//!
//! ```text
//!  sources ──newest frame──► engine thread (master clock, one tick per output frame)
//!     │                        │  tracker step per new frame ─► subject store (per source)
//!     │                        │  catalog: wide + subject × preset shots ─► framers
//!     │                        │  switcher ◄── commands      director ─► suggestion / take
//!     │                        │  compositor ─► program frame ─► encoder (RTMP + MP4)
//!     │                        │  snapshot ─► Latest<Arc<EngineSnapshot>> ─► UI, headless
//!     │                        ▼
//!     └──────────────► discovery worker per source (detector at ~1 Hz, newest frame only)
//!                              └─ kind = "sidecar": every worker's detector is a client of
//!                                 one shared detector process (ac-sidecar, ADR 0004)
//! ```
//!
//! * [`EngineCore`] is the deterministic heart: given frames, discovery
//!   results and commands it computes subjects, shots, framing, switching,
//!   the director's choices and the program picture. It has no threads and
//!   reads no clock, so it is tested tick by tick.
//! * [`Engine`] is the runtime around it: it opens the sources, each under
//!   a supervisor that reopens it when its input is lost
//!   ([`ac_sources::open_supervised`]), runs the discovery workers,
//!   drives the core from a master clock on its own thread, owns the program
//!   encoder, accepts [`EngineCommand`]s over a channel and publishes an
//!   immutable [`EngineSnapshot`] every tick. See [`runtime`] for the
//!   threads, channels and mailboxes.
//! * [`StudioConfig`] describes a studio (sources, queries, detector,
//!   output, director, framing) as TOML or JSON, in a layout named by its
//!   `schema` key ([`SCHEMA_VERSION`]).
//! * [`readiness`] checks a studio before a show without starting it: the
//!   problems in the config's values, and the files, tools and folders it
//!   names (`auto-camera --check`).
//! * [`trace`] carries one record per tick (subjects, shot targets and
//!   crops, program and preview) from the engine thread to a writer, such
//!   as the app's `--trace` file, for scoring with `ac_eval`.
//! * [`alarms`] reads the snapshots a reader takes into a persistent list
//!   of what is wrong on air (spec M14): program black, frozen or on a
//!   lost subject, encoder drops, a failed output, a stream reconnecting,
//!   stalled or down, a recording without audio. It is pure: each reader
//!   keeps its own, and acknowledges alarms there.
//! * [`queries`] reads each query's prompt once, when it is created: the
//!   detector is asked for the parsed phrase ("speaker upper body" asks for
//!   "a person speaking"), and the prompt's shot words pick its framings
//!   unless presets are given (ADR 0003).
//!
//! # Time
//!
//! Two clocks, never mixed: each source's **media clock** (frame PTS) drives
//! tracking and subject identity; the engine's **master clock** (tick index ×
//! frame period) drives framing, transitions, the director and the encoder.
//! The master clock is paced by an injectable [`ac_sources::Clock`]: a wall
//! clock in production, a sped-up clock in tests. Discovery workers always
//! run in real time, so a clock that runs much faster than the detector makes
//! every detection arrive too old to apply; the [`EngineCore`] itself
//! takes `now` as an argument and is tested on simulated time directly.

pub mod alarms;
pub mod audio;
pub mod catalog;
pub mod command;
pub mod config;
pub mod core;
pub mod detectors;
pub mod discovery;
pub mod mailbox;
pub mod output;
pub mod queries;
pub mod readiness;
pub mod runtime;
pub mod schedule;
pub mod snapshot;
pub mod trace;
pub mod vision;

pub use alarms::{Alarm, AlarmCause, AlarmChange, Alarms};
pub use catalog::{ShotStatus, subject_shot_id, wide_shot_id};
pub use command::{CommandError, CommandReply, EngineCommand};
pub use config::{
    ConfigError, DetectorSettings, EncoderSetting, LoadedConfig, MonitorSettings, OutputSettings,
    PresetSpec, QuerySettings, SCHEMA_VERSION, SidecarSettings, SourceEntry, StudioConfig,
    preset_for,
};
pub use core::{EngineCore, SourceFeed, TickOutput};
pub use queries::{PromptReader, build_query, default_presets, read_prompt, shot_presets};
pub use readiness::{Readiness, SystemInspector};
pub use runtime::{Engine, EngineError, EngineHandle, EngineOptions, EngineReport, PendingReply};
pub use snapshot::{
    DirectorView, EngineSnapshot, KeepAwakeState, MatchView, OutputState, OutputView, PassOutcome,
    QueryView, ShotView, SourceView, SubjectView, SwitcherView, Tally, TimingView,
};
pub use trace::{TraceRecord, TraceSender};
