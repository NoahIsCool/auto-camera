//! The engine core: one deterministic step of the whole pipeline per tick.
//!
//! [`EngineCore`] owns every piece of studio state that is not I/O: per
//! source the [`SourceVision`] (subject store and tracker), the query list,
//! the shot [`Catalog`] with one framer per shot, the [`Switcher`], the
//! [`Director`] and the [`Compositor`]. It does not own sources, detector
//! threads or the encoder; the runtime ([`crate::runtime`]) feeds it the
//! newest frame of every source and the discovery results that arrived, and
//! takes back the program frame and the discovery requests to send.
//!
//! One [`EngineCore::tick`] at master-clock time `now`:
//!
//! 1. **Protection.** The subjects of the program and preview shots are
//!    pushed to their stores, so on-air subjects are never retired.
//! 2. **Discovery results** free their source's request slot (unless a
//!    newer request replaced a timed-out one) and are applied unless stale
//!    ([`crate::discovery::staleness`]). Each applied result also becomes
//!    the prompt feedback of every live query it looked for on that source
//!    (spec M2): how many detections it returned, how many of them reach
//!    the subject store's floor (only those can become shots) and the best
//!    score ([`crate::snapshot::MatchView`]), tagged with the frame's
//!    sequence number and PTS. Stale and failed results change no
//!    feedback; they are recorded as the source's newest outcome
//!    ([`crate::snapshot::PassOutcome`]), so the window can say the
//!    feedback is no longer current.
//! 3. **Frames.** Each source's newest frame, if new, advances its tracker
//!    and store (media time = the frame's PTS).
//! 4. **Catalog.** The shot list is re-derived from the stores and queries.
//! 5. **Framing.** Every shot's framer steps by `dt` and is rescored.
//! 6. **Switching.** The switcher advances its transition; when one ends,
//!    a take the operator queued during it fires if the catalog still has
//!    its shot available, and is dropped with the reason otherwise
//!    ([`ac_switcher`]). The director then proposes (and in Auto mode
//!    takes) a shot, weighing each shot's quality and its subject's query
//!    priority; a shot the operator staged holds preview against the
//!    director's suggestions until it is taken or released
//!    ([`directing`]).
//! 7. **Rendering.** The compositor renders the program; monitors and
//!    multiview tiles are rendered at their own, lower rates.
//! 8. **Discovery requests** are issued for sources that are due (at most
//!    `discovery.rate_hz` each, never while one is in flight), with each
//!    source's newest frame, asking for each query's parsed detector
//!    phrase ([`Query::detector_phrase`]), not its prompt. When one
//!    detector process serves every source (the sidecar,
//!    [`crate::config::DetectorSettings::is_shared`]), the studio keeps
//!    one request in flight in all, and the due source that waited longest
//!    since its last turn goes next. Requests then never queue behind each
//!    other's passes, so an answer is about one pass old rather than one
//!    pass per source (two sources at 1.05 s a pass would otherwise get
//!    every answer back 2.1 s old, past the 2 s age limit), and each
//!    source's rate falls to what the process can serve: with N sources, a
//!    pass every N pass times at most.
//!
//! A source whose detector is still loading (the runtime says so in its
//! [`SourceFeed`]) is *preparing*: it gets no requests and counts no
//! timeouts, since a detector that has not loaded cannot answer. The tick
//! on which loading is reported over makes it due at once, so the newest
//! frame goes out on that same tick, and records that tick's master-clock
//! time as [`DiscoveryStats::ready_after`]. A source starts preparing when
//! it has a detector, so the snapshot taken before the first tick already
//! says so. A shared detector is one detector for the whole studio: while
//! the sidecar's first start is still running (any source's loader still
//! waits for its hello or its failure), every source is preparing, and they
//! all become ready on the same tick, so the sources start taking turns
//! only once that start has ended, the first source first; after a failed
//! start each turn's pass fails with the reason. The loading state is tick
//! input like the
//! frames, so the core stays a deterministic function of its inputs.
//!
//! # Sources that drop out
//!
//! The runtime's sources are supervised (`ac_sources::supervisor`): one that
//! loses its input reports [`SourceState::Offline`] or
//! [`SourceState::Reconnecting`], with the reason in [`SourceFeed::error`],
//! and comes back under a new *epoch* (see `ac_media`), its sequence numbers
//! and PTS continuing where they left off. The core only compares epochs:
//!
//! * a source that is not delivering is unavailable at once, so its shots
//!   cannot be taken; its last picture stays where it is shown;
//! * a discovery answer about a frame of an older epoch than the source's
//!   newest frame is stale ([`crate::discovery::Staleness::OldEpoch`]): it
//!   describes the picture of a connection that is gone, however recent it
//!   is in media time;
//! * a source that was missing when the engine started is described by a
//!   placeholder ([`crate::config::SourceEntry::placeholder_info`]); its
//!   first frame gives the core its real picture size, and the description
//!   of the connection that delivered it ([`SourceFeed::info`]) its frame
//!   rate. A source's description is otherwise fixed: a later connection is
//!   taken to have the first one's.
//!
//! Given the same configuration, frames, results and commands, the core
//! produces the same program, which is what the deterministic tests rely on,
//! and the same per-tick trace record ([`EngineCore::trace_record`]).
//! Commands are applied between ticks by [`EngineCore::apply`].

use std::collections::HashMap;
use std::time::Duration;

use ac_compositor::{Compositor, CompositorConfig};
use ac_core::{Query, QueryId, ShotId, SourceId};
use ac_director::{Director, DirectorDecision};
use ac_framing::FramerConfig;
use ac_media::{SourceInfo, SourceState, VideoFrame};
use ac_switcher::Switcher;

use crate::catalog::Catalog;
use crate::config::{DiscoverySettings, MonitorSettings, QuerySettings, Ranking, StudioConfig};
use crate::discovery::{DiscoveryRequest, DiscoveryResult};
use crate::queries::PromptReader;
use crate::schedule::RateMeter;
use crate::snapshot::{DiscoveryStats, MatchView};
use crate::vision::SourceVision;

mod commands;
mod directing;
mod passes;
mod render;
mod shots;
mod show;
mod snapshot;
#[cfg(test)]
mod tests;
mod trace;

/// A source counts as unavailable when no new frame arrived for this long.
pub const SOURCE_STALE_AFTER: Duration = Duration::from_secs(1);

/// What the runtime knows about a source on one tick.
#[derive(Debug, Clone)]
pub struct SourceFeed {
    /// Health reported by the source.
    pub state: SourceState,
    /// The source's newest frame (possibly the same as last tick).
    pub frame: Option<VideoFrame>,
    /// The source's detector is still loading (false without a detector);
    /// see the module docs.
    pub detector_loading: bool,
    /// Why the source is not delivering, when an error is the reason: what
    /// stopped it, or what keeps it offline while it reconnects.
    pub error: Option<String>,
    /// The description of what the source delivers now
    /// (`VideoSource::current_info`), read after `frame`, so it is that
    /// frame's connection's or a newer one's; `None` when not known. The
    /// core takes it with the source's first frame only (see the module
    /// docs).
    pub info: Option<SourceInfo>,
}

/// What a tick produced for the runtime.
#[derive(Debug, Clone)]
pub struct TickOutput {
    /// The program frame for the encoder.
    pub program: VideoFrame,
    /// Discovery requests to hand to the workers, by source index.
    pub requests: Vec<(usize, DiscoveryRequest)>,
}

/// Discovery scheduling of one source.
#[derive(Debug, Clone, Default)]
struct DiscoveryState {
    /// The source has a detector worker.
    enabled: bool,
    /// Earliest time of the next request.
    next_due: Duration,
    /// When the request in flight was sent.
    in_flight_since: Option<Duration>,
    /// When the newest request was sent, for taking turns on a shared
    /// detector.
    last_sent: Option<Duration>,
    /// Sequence number of the newest frame requested.
    last_requested: Option<u64>,
    /// Sequence number of the newest frame whose results were applied.
    last_applied: Option<u64>,
    /// Prompt feedback: per live query, what the newest applied result
    /// that looked for it found.
    matches: HashMap<QueryId, MatchView>,
    /// Counters, and whether the detector is still preparing.
    stats: DiscoveryStats,
}

impl DiscoveryState {
    /// The state of a source that runs discovery when `enabled`: it
    /// prepares until a tick says its detector has loaded.
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            stats: DiscoveryStats {
                preparing: enabled,
                ..DiscoveryStats::default()
            },
            ..Self::default()
        }
    }

    /// Takes in whether the detector is still `loading` at `now`; when it
    /// stops, the source is ready and due at once (see the module docs).
    fn note_loading(&mut self, loading: bool, now: Duration) {
        if !self.enabled {
            return;
        }
        if loading {
            self.stats.preparing = true;
        } else if self.stats.preparing {
            self.stats.preparing = false;
            self.stats.ready_after = Some(now);
            self.next_due = now;
        }
    }
}

/// One source as the core sees it.
#[derive(Debug)]
struct SourceSlot {
    /// Stable id.
    id: SourceId,
    /// Operator-facing name.
    name: String,
    /// The source's description: as given to [`EngineCore::new`], with the
    /// size and rate of its first frame's connection once one arrived (see
    /// [`SourceSlot::describe`]).
    info: SourceInfo,
    /// Picture aspect (width / height).
    aspect: f64,
    /// Identity and tracking.
    vision: SourceVision,
    /// Newest frame seen.
    frame: Option<VideoFrame>,
    /// Health reported by the source.
    state: SourceState,
    /// Why the source is not delivering, as reported with `state`.
    error: Option<String>,
    /// Master-clock time of the newest new frame.
    last_frame_at: Option<Duration>,
    /// New frames seen.
    frames: u64,
    /// Frames the source produced that the engine never saw (sequence gaps:
    /// the mailbox kept only the newest).
    missed: u64,
    /// New-frame rate.
    rate: RateMeter,
    /// Discovery scheduling.
    discovery: DiscoveryState,
}

impl SourceSlot {
    /// Takes the description of a source's first frame, `frame`, and of the
    /// connection that delivered it, `connection` (see the module docs): a
    /// source missing at start was given a placeholder, and nothing has been
    /// tracked or framed on its picture yet. The size comes from the frame,
    /// the rate from the connection. An empty size, a description of another
    /// source and a rate that is not positive and finite are ignored.
    fn describe(&mut self, frame: &VideoFrame, connection: Option<&SourceInfo>) {
        let size = (frame.width, frame.height);
        if size != (self.info.width, self.info.height) && size.0 > 0 && size.1 > 0 {
            self.info.width = frame.width;
            self.info.height = frame.height;
            self.aspect = frame.aspect();
        }
        let usable = |c: &&SourceInfo| c.id == self.id && c.fps.is_finite() && c.fps > 0.0;
        if let Some(c) = connection.filter(usable) {
            self.info.fps = c.fps;
        }
    }

    /// True when the source is delivering and has a fresh frame at `now`.
    fn available(&self, now: Duration) -> bool {
        matches!(self.state, SourceState::Starting | SourceState::Running)
            && self
                .last_frame_at
                .is_some_and(|t| now.saturating_sub(t) <= SOURCE_STALE_AFTER)
    }
}

/// The deterministic engine core; see the module docs.
#[derive(Debug)]
pub struct EngineCore {
    /// Sources in config order.
    sources: Vec<SourceSlot>,
    /// Prompt queries in creation order, each prompt read once when the
    /// query was created ([`crate::queries`]).
    queries: Vec<Query>,
    /// The director priority of each live query, fixed when the query was
    /// created: from the config by its [`Ranking`], or, for a query added
    /// by command, as if it were listed last. Kept in step with `queries`
    /// by the commands that add and remove them.
    priorities: HashMap<QueryId, u8>,
    /// How the config ranks queries that give no priority, which also
    /// ranks the queries added by command.
    ranking: Ranking,
    /// How prompts of queries added by command are read: the studio's
    /// lexicon and what its detector sees ([`crate::queries`]).
    prompts: PromptReader,
    /// Discovery tuning.
    discovery: DiscoverySettings,
    /// One detector process serves every source, one pass at a time (the
    /// sidecar): the studio keeps one request in flight in all.
    shared_detector: bool,
    /// Framing tuning for new shots.
    framing: FramerConfig,
    /// Monitor and tile rendering.
    monitors: MonitorSettings,
    /// Live shots.
    catalog: Catalog,
    /// Preview/program state.
    switcher: Switcher,
    /// Auto-director.
    director: Director,
    /// The director's newest decision.
    decision: DirectorDecision,
    /// Suggestion last staged on preview, or passed over for the
    /// operator's hold, in suggest mode (see [`directing`]).
    last_staged: Option<ShotId>,
    /// The shot the operator staged, which holds preview against the
    /// director's suggestions until it is taken or released (see
    /// [`directing`]).
    preview_hold: Option<ShotId>,
    /// Suggestions the director did not stage because of the operator's
    /// hold.
    held_back: u64,
    /// The newest of them.
    last_held_back: Option<ShotId>,
    /// Takes requested by the director.
    director_takes: u64,
    /// Program renderer.
    compositor: Compositor,
    /// Master-clock time of the newest tick.
    now: Duration,
    /// Ticks run.
    ticks: u64,
    /// Program shot as of the newest bookkeeping.
    program: Option<ShotId>,
    /// When the program shot went on air.
    program_since: Duration,
    /// Times the program shot changed.
    program_changes: u64,
    /// Newest program frame.
    program_frame: Option<VideoFrame>,
    /// Newest program monitor picture.
    program_monitor: Option<VideoFrame>,
    /// Newest preview monitor picture.
    preview_monitor: Option<VideoFrame>,
    /// Newest multiview tiles.
    thumbnails: HashMap<ShotId, VideoFrame>,
    /// Earliest time of the next monitor refresh.
    next_monitor_at: Duration,
    /// Earliest time of the next tile refresh.
    next_thumbs_at: Duration,
    /// Shots drawn black on the newest program frame.
    render_issues: Vec<String>,
    /// The sources and default recording path, as configured, for the
    /// show export ([`show`]).
    show_base: show::ShowBase,
}

impl EngineCore {
    /// A core for `config` whose sources are described by `infos` (same
    /// order) and of which those with `detectors[i]` run discovery.
    pub fn new(
        config: &StudioConfig,
        infos: Vec<SourceInfo>,
        detectors: &[bool],
    ) -> Result<Self, String> {
        config.validate().map_err(|e| e.to_string())?;
        if infos.len() != config.sources.len() {
            return Err("one source description per configured source is required".into());
        }
        let store = config.store_config().map_err(|e| e.to_string())?;
        let mut sources = Vec::new();
        for (i, (entry, info)) in config.sources.iter().zip(infos).enumerate() {
            if info.width == 0 || info.height == 0 {
                return Err(format!("source {:?} reports an empty picture", entry.id));
            }
            let id = entry.source_id();
            sources.push(SourceSlot {
                vision: SourceVision::new(
                    id.clone(),
                    store,
                    config.tracker.clone(),
                    config.discovery.max_undetected(),
                )?,
                id,
                name: entry.display_name().to_string(),
                aspect: f64::from(info.width) / f64::from(info.height),
                info,
                frame: None,
                state: SourceState::Starting,
                error: None,
                last_frame_at: None,
                frames: 0,
                missed: 0,
                rate: RateMeter::new(Duration::from_secs(1)),
                discovery: DiscoveryState::new(detectors.get(i).copied().unwrap_or(false)),
            });
        }
        let queries = config.build_queries().map_err(|e| e.to_string())?;
        let ranking = Ranking::of(&config.queries);
        let priorities = config
            .queries
            .iter()
            .map(QuerySettings::query_id)
            .zip(ranking.priorities(&config.queries))
            .collect();
        let mut switcher = Switcher::new();
        switcher
            .set_transition(config.switcher.to_transition())
            .map_err(|e| e.to_string())?;
        let director = Director::new(config.director_config().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let compositor = Compositor::new(CompositorConfig::new(
            config.output.width,
            config.output.height,
        ))
        .map_err(|e| e.to_string())?;
        let mut core = Self {
            sources,
            queries,
            priorities,
            ranking,
            prompts: PromptReader::of(config),
            discovery: config.discovery,
            shared_detector: config.detector.is_shared(),
            framing: config.framing,
            monitors: config.monitors,
            catalog: Catalog::new(),
            switcher,
            director,
            decision: DirectorDecision::off(),
            last_staged: None,
            preview_hold: None,
            held_back: 0,
            last_held_back: None,
            director_takes: 0,
            compositor,
            now: Duration::ZERO,
            ticks: 0,
            program: None,
            program_since: Duration::ZERO,
            program_changes: 0,
            program_frame: None,
            program_monitor: None,
            preview_monitor: None,
            thumbnails: HashMap::new(),
            next_monitor_at: Duration::ZERO,
            next_thumbs_at: Duration::ZERO,
            render_issues: Vec::new(),
            show_base: show::ShowBase::of(config),
        };
        // Wide shots exist from the start, so they can be staged before the
        // first tick.
        core.sync_catalog();
        Ok(core)
    }

    /// Ticks run so far.
    pub fn ticks(&self) -> u64 {
        self.ticks
    }

    /// Master-clock time of the newest tick.
    pub fn now(&self) -> Duration {
        self.now
    }

    /// The live queries.
    pub fn queries(&self) -> &[Query] {
        &self.queries
    }

    /// The live shots.
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// The switcher.
    pub fn switcher(&self) -> &Switcher {
        &self.switcher
    }

    /// The director's newest decision.
    pub fn decision(&self) -> &DirectorDecision {
        &self.decision
    }

    /// Ids of the sources, in order.
    pub fn source_ids(&self) -> Vec<SourceId> {
        self.sources.iter().map(|s| s.id.clone()).collect()
    }

    /// The subject store of source `index`.
    pub fn store(&self, index: usize) -> Option<&ac_subjects::SubjectStore> {
        self.sources.get(index).map(|s| s.vision.store())
    }

    /// Runs one tick at master-clock time `now`; see the module docs.
    pub fn tick(
        &mut self,
        now: Duration,
        feeds: &[SourceFeed],
        results: Vec<DiscoveryResult>,
    ) -> TickOutput {
        let dt = if self.ticks == 0 {
            0.0
        } else {
            now.saturating_sub(self.now).as_secs_f64()
        };
        self.now = now;
        self.ticks += 1;
        self.update_protection();
        for r in results {
            self.apply_result(r);
        }
        // A shared detector is ready for every source or for none.
        let studio_loading = self.shared_detector
            && feeds
                .iter()
                .zip(&self.sources)
                .any(|(f, s)| s.discovery.enabled && f.detector_loading);
        for (i, feed) in feeds.iter().enumerate().take(self.sources.len()) {
            self.ingest_feed(i, feed, studio_loading);
        }
        self.sync_catalog();
        self.step_framing(dt);
        let catalog = &self.catalog;
        self.switcher.tick(dt, |id| catalog.is_available(id));
        self.note_program();
        self.run_director();
        self.note_program();
        let program = self.render();
        TickOutput {
            program,
            requests: self.schedule_discovery(),
        }
    }

    /// Takes a source's health and newest frame and advances its vision when
    /// the frame is new, and whether its detector is still loading: its own
    /// loader, or, when `studio_loading`, the shared detector's (see the
    /// module docs). The first frame of a source sets its picture size and
    /// rate ([`SourceSlot::describe`]).
    fn ingest_feed(&mut self, i: usize, feed: &SourceFeed, studio_loading: bool) {
        let now = self.now;
        let s = &mut self.sources[i];
        s.state = feed.state;
        s.error.clone_from(&feed.error);
        s.discovery
            .note_loading(feed.detector_loading || studio_loading, now);
        let Some(frame) = &feed.frame else {
            return;
        };
        if frame.source != s.id || s.frame.as_ref().is_some_and(|f| frame.seq <= f.seq) {
            return;
        }
        if s.frame.is_none() {
            s.describe(frame, feed.info.as_ref());
        }
        let expected = s.frame.as_ref().map_or(0, |f| f.seq + 1);
        s.missed += frame.seq.saturating_sub(expected);
        s.frame = Some(frame.clone());
        s.last_frame_at = Some(now);
        s.frames += 1;
        s.rate.record(now);
        s.vision.on_frame(frame);
    }

    /// Index of the source with `id`.
    fn source_index(&self, id: &SourceId) -> Option<usize> {
        self.sources.iter().position(|s| &s.id == id)
    }
}
