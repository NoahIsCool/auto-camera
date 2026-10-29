//! The detection log: how the engine hands every discovery pass it applied
//! to whoever writes it down, for replaying the run's detections later
//! (`ac_vision::replay`).
//!
//! The core hands back a [`PassRecord`] for each result a tick applied
//! ([`TickOutput::applied`](crate::TickOutput::applied)): the frame's
//! source, sequence number and PTS, the queries the pass looked for and its
//! detections with box, score and extent. The engine thread sends them, in
//! the order applied, on the [`PassSender`] of its
//! [`EngineOptions::detections`](crate::EngineOptions::detections); the
//! receiver, such as the app's `--record-detections` writer, runs on a
//! thread of its own.
//!
//! Delivery follows the trace's rules ([`crate::trace`]): a bounded
//! `std::sync::mpsc::sync_channel` that the engine thread never waits on. A
//! full channel drops the pass, so the master clock never waits for a disk
//! (the gap shows as fewer records than the snapshot's applied passes); a
//! closed one ends the log for the rest of the run. The engine's sender
//! goes with its thread, so a writer that reads until the channel closes
//! has every record once the engine has shut down.

use std::sync::mpsc::SyncSender;

pub use ac_vision::PassRecord;

/// The sending end of a detection log channel; see the module docs.
pub type PassSender = SyncSender<PassRecord>;
