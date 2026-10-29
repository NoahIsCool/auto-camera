//! The eval runner: the engine stepped over a clip on simulated time, with
//! live or replayed detections, writing the per-tick trace.
//!
//! Scoring framing (spec V9) compares traces of runs over the same input,
//! so a run must be repeatable to the byte. The app is not: its engine
//! thread, sources and discovery workers run on the wall clock, and when a
//! pass lands depends on the machine. This runner drives the deterministic
//! [`EngineCore`](ac_engine::EngineCore) itself, on one thread, on a
//! [`VirtualClock`](ac_sources::VirtualClock):
//!
//! * frames come from the studio's sources read in step with the ticks
//!   ([`inputs`]): a synthetic pattern, or a file decoded frame by frame
//!   (with its `start_s` and `duration_s` segment);
//! * discovery requests are answered at once and handed back a fixed
//!   number of ticks later ([`run`]), by the studio's own detector or by a
//!   detection log played back ([`detectors`]);
//! * every tick's trace record (`ac_eval::trace`) goes to the trace, and
//!   every applied pass, given a path, to a new detection log.
//!
//! Recording a run's detections once (with a model, say) and replaying
//! them gives the same trace without the model, every time; that is how
//! CI scores framing without running inference.
//!
//! # Invariants
//!
//! * Equal config, inputs and detections give byte-equal traces, and the
//!   replay of a run's own detection log gives that run's trace.
//! * Only a virtual clock reaches the run; no wall clock, timer or sleep is
//!   used in this crate (enforced by [`Runner::run`]'s signature and by
//!   `tests/clock.rs`).
//! * The library writes only to the writers it is given; the binary
//!   (`main.rs`) creates the files, and never overwrites one.

pub mod cli;
pub mod detectors;
pub mod inputs;
pub mod run;

pub use cli::Cli;
pub use detectors::Detections;
pub use inputs::Input;
pub use run::{RunError, RunOptions, RunReport, Runner, SourceReport};
