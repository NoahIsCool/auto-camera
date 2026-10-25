//! Stable subject identities for one video source.
//!
//! Vision works on two time scales. A slow open-vocabulary detector (0.5–2
//! Hz) says *what* is in the picture: boxes for each text prompt. A fast
//! per-frame tracker (15 Hz to video rate) says *where* each known subject is
//! now. Neither gives identity on its own: the detector has no memory and the
//! tracker drifts. The [`SubjectStore`] fuses the two into stable subjects
//! that shots can be built on:
//!
//! * detections are associated to subjects with optimal (Hungarian)
//!   assignment against motion-predicted boxes, and re-anchor them;
//! * track observations move subjects between detections but never create
//!   or revive one;
//! * a detection restarts a subject's tracker when the tracker drifted
//!   (the detection disagrees with the box), failed to start, or was held
//!   back by its contrast check, with no confident observation, since the
//!   subject's previous detection ([`record`]), so a tracker that cannot
//!   find its subject again is not left coasting while detections keep the
//!   subject alive; a tracker that coasts because something covers its
//!   subject is not restarted for it, so its template, taken before the
//!   occluder came, finds the subject again when it leaves;
//! * prompts that find the same entity become aliases of one subject at
//!   once, while an alias frames shots only after it has been seen in
//!   several detection passes and stops after several passes without it
//!   ([`labels`]), so one stray box never makes a lasting alias shot. The
//!   primary query, chosen at admission, is outside that rule and always
//!   frames; removing it promotes an alias that frames, and only when none
//!   does an unconfirmed one, which then frames at once;
//! * admission is fair across prompts and bounded by a soft cap (hard cap
//!   [`HARD_CAP`]);
//! * operator intent wins: pinned, hidden and on-air (protected) subjects are
//!   never retired automatically, and a reacquire keeps the subject's id.
//!
//! The store is pure and deterministic. Time is the source's media clock
//! (`Duration` PTS) passed into every call, every change is returned as a
//! [`StoreEvent`], and replaying the same calls yields the same events. It
//! owns all identity state for its source; the engine drives it from one
//! task and publishes snapshots of [`SubjectStore::records`].
//!
//! The engine's contract:
//!
//! 1. Feed each detector batch with [`SubjectStore::ingest_detections`] and
//!    each tracker step with [`SubjectStore::ingest_tracks`], using the PTS of
//!    the frame they were computed on. PTS must not go backwards across a
//!    source restart; start a new store instead. Pass the tracker's
//!    low-confidence observations too, with whether the tracker held them
//!    back ([`TrackObservation::held`]): they never move a box, but the held
//!    ones are what tells the store that a tracker coasts and needs a
//!    restart.
//! 2. Call [`SubjectStore::tick`] once per frame.
//! 3. Act on tracker events: start or restart a tracker on
//!    [`StoreEvent::NeedsTrackerStart`] and drop the previous tracker's
//!    pending observations; stop it on [`StoreEvent::NeedsTrackerStop`].
//!    Report a start that failed with [`SubjectStore::tracker_failed`], so
//!    the next matching detection retries it.
//! 4. Push the switcher's program and preview subjects with
//!    [`SubjectStore::set_protected`] whenever they change.
//! 5. Stop sending detections for a query before calling
//!    [`SubjectStore::remove_query`].
//! 6. Make each subject's shots from [`SubjectRecord::shot_queries`], not
//!    from every query it answers ([`SubjectRecord::queries`]).

mod assignment;
pub mod associate;
pub mod config;
pub mod event;
pub mod labels;
pub mod observation;
pub mod record;
pub mod store;

pub use config::{HARD_CAP, StoreConfig};
pub use event::{RetireReason, StoreError, StoreEvent};
pub use labels::LabelEvidence;
pub use observation::{Detection, TrackObservation};
pub use record::SubjectRecord;
pub use store::SubjectStore;

#[cfg(test)]
mod testkit;
