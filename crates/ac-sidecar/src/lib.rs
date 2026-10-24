//! Out-of-process detectors for Auto Camera: SAM 3.1 on MLX first.
//!
//! # Why a sidecar
//!
//! SAM 3.1 finds the right person for a role phrase ("a person speaking":
//! the speaker in 10 of 20 labelled frames at 0.5, 14 of 20 at 0.3, with no
//! false picks; measured on Noah's M3 Max, `sam31-vs-owlv2.md`), and the
//! only SAM 3.x runtime ever measured on that Mac is mlx-vlm, in Python.
//! Noah chose to run it as a sidecar process under Meta's SAM License,
//! retired only if an in-process ONNX build meets the design's R1 exit
//! criteria (`model-picker-design.md` §4.6). This crate is the Rust side:
//! it starts the Python process, talks to it, and presents it as an
//! ordinary [`ac_vision::Detector`], so nothing downstream knows a model
//! runs in another process.
//!
//! # Layers
//!
//! * [`protocol`] (pure): protocol v1, length-prefixed JSON headers with a
//!   binary payload over the child's stdin and stdout; `hello`, `detect`,
//!   `result`, `error`; strict validation.
//! * [`prep`] (pure): a frame's RGBA to RGB8, box-filtered down to a
//!   configurable longest side (1008 px by default), but never with its
//!   shorter side below the model's input side from its hello.
//! * [`host`]: [`SamHost`], the single owner of one child process, its pipe
//!   threads, its deadlines (60 s for the hello, one per request), its
//!   stale-reply filter and its restart backoff.
//! * [`service`]: [`SidecarService`], a thread that owns the host so every
//!   source's detector shares one process (one SAM process per machine,
//!   design §4.6), and [`SidecarClient`], the non-owning handle detectors
//!   use.
//! * [`detector`]: [`SidecarDetector`], the [`ac_vision::Detector`]: groups
//!   queries by phrase, applies per-query thresholds, marks people as
//!   whole people, and maps boxes back to queries.
//!
//! # Ownership and threads
//!
//! ```text
//! discovery thread ──detect──▶ SidecarDetector ──job──▶ ac-sam-host thread (owns SamHost)
//!   (one per source)            (SidecarClient)          │ owns the Child
//!                                                         ├─ ac-sam-write: owns stdin  ◀─ bounded(1)
//!                                                         ├─ ac-sam-read:  owns stdout ─▶ bounded events
//!                                                         └─ ac-sam-log:   owns stderr ─▶ log ring
//! ```
//!
//! Every piece of state has one owner; threads exchange messages over
//! bounded channels; the log ring is the only shared structure, and its
//! lock is never held across I/O. One request is in flight at a time.
//! Every frame sent and every answer carries the source id, sequence
//! number and PTS of the frame it describes; an answer that does not match
//! the outstanding request is dropped and counted.
//!
//! # Not here (yet)
//!
//! Masks, parts, per-query model choice, lanes, TCP to a GPU node and the
//! SAM video tracker are later phases (design §5); protocol v1 reserves
//! room for masks.

mod backoff;
mod child;
pub mod detector;
pub mod host;
mod logs;
pub mod prep;
pub mod protocol;
pub mod service;

pub use detector::{DEFAULT_REQUEST_TIMEOUT, DEFAULT_THRESHOLD, DetectorConfig, SidecarDetector};
pub use host::{
    DetectReply, DetectRequest, Freshness, HostConfig, HostError, HostState, HostStats, HostStatus,
    SamHost, StopHandle,
};
pub use prep::{DEFAULT_SEND_SIDE, RgbImage, prepare, send_size};
pub use protocol::{Capabilities, Found, Hello, Phrase, ProtocolError};
pub use service::{SidecarClient, SidecarService};
