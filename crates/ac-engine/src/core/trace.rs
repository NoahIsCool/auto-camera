//! Building the per-tick [`TraceRecord`] from the core's state.
//!
//! The record describes the newest tick, read after it ran, the way the
//! snapshot does ([`super::snapshot`]), but as plain data for scoring
//! instead of for the operator: each source's newest frame and its subjects
//! (ascending id, from the store), each shot's subject, ideal crop
//! ([`ShotEntry::ideal`](crate::catalog::ShotEntry::ideal)) and crop in
//! display order, and the switcher's program, preview and output picture.
//!
//! It is a function of the core's state alone, which is a function of its
//! inputs (frames, discovery results and commands, with time entering as
//! the tick's media time), so equal inputs give byte-equal records. Nothing
//! is read from a map, whose order could vary. The runtime's clock facts,
//! whether the tick was late and how many ticks were skipped before it, are
//! not the core's to know: the record leaves them `false` and 0 for the
//! runtime to fill in ([`crate::trace`]).

use ac_eval::trace::{
    FrameTag, ShotTrace, SourceTrace, SubjectTrace, SwitcherTrace, TRACE_VERSION, TraceRecord,
    TransitionTrace,
};

use super::EngineCore;

impl EngineCore {
    /// The trace record of the newest tick; see the module docs. Before the
    /// first tick it describes the initial state as tick 0.
    pub fn trace_record(&self) -> TraceRecord {
        let sources = self
            .sources
            .iter()
            .map(|s| SourceTrace {
                id: s.id.clone(),
                frame: s.frame.as_ref().map(|f| FrameTag {
                    seq: f.seq,
                    pts: f.pts,
                }),
                subjects: s
                    .vision
                    .store()
                    .records()
                    .map(|r| SubjectTrace {
                        id: r.subject.id,
                        query: r.subject.query.clone(),
                        bbox: r.subject.bbox,
                        state: r.subject.state,
                    })
                    .collect(),
            })
            .collect();
        let shots = self
            .catalog
            .entries()
            .iter()
            .map(|e| ShotTrace {
                id: e.shot.id.clone(),
                source: self.sources[e.source].id.clone(),
                subject: e.subject,
                target: e.ideal,
                crop: e.crop,
            })
            .collect();
        let transition = self.switcher.in_transition().then(|| {
            let out = self.switcher.output();
            TransitionTrace {
                base: out.base,
                overlay: out.overlay.map_or(0.0, |(_, opacity)| opacity),
                dip: out.dip,
            }
        });
        TraceRecord {
            version: TRACE_VERSION,
            tick: self.ticks,
            pts: self.now,
            late: false,
            skipped: 0,
            sources,
            shots,
            switcher: SwitcherTrace {
                program: self.switcher.program().cloned(),
                preview: self.switcher.preview().cloned(),
                transition,
            },
        }
    }
}
