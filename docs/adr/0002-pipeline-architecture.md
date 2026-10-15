# ADR 0002: Pipeline architecture

Status: accepted (2026-10-15)

## Context

The pipeline crates (sources, vision, subjects, framing, director, switcher,
compositor, output) are pure or adapter crates with no knowledge of each
other. Something has to run them together in real time: deliver frames,
run a slow detector next to a fast tracker, keep shots framed, switch,
render and encode program, and let an operator (a window, or a headless
runner) watch and command all of it. The proof of concept did this with
one orchestrator, shared locks and unbounded queues, and it stalled and
leaked under load. The rules from CLAUDE.md apply: one owner per piece of
state, message passing and immutable snapshots instead of shared locks,
latest-frame mailboxes, stale results dropped, every frame tagged with
source, sequence number and PTS.

## Decision

`crates/ac-engine` runs the pipeline. All mutable studio state lives on
**one engine thread**; everything else talks to it through mailboxes and
channels, and the operator sees it only through immutable snapshots.

### Threads

| Thread | Owner | Job |
|---|---|---|
| UI (`main`) or headless runner | `apps/auto-camera` | Reads snapshots, posts commands. Never touches engine state. |
| `ac-engine` | `Engine` | The master clock loop: applies commands, runs one `EngineCore::tick` per output frame, pushes program to the encoder, publishes a snapshot. |
| source workers (`ac-synthetic-<id>`, file decoders) | each `VideoSource` | Decode or generate frames at the source rate; publish into the source's frame mailbox and a small bounded audio queue. |
| `ac-discovery-<source>` | `DiscoveryWorker`, one per source with a detector | Runs the open-vocabulary detector on the newest frame it was handed. |
| `ac-output-start`, `ac-output-finish` | `OutputController` | Start ffmpeg (probing encoders) and finish it (flushing the recording) off the engine thread, since both can take seconds. |
| `ac-output-video/-audio/-progress/-stderr` and the `ffmpeg` child | `ProgramEncoder` | Feed and watch the single program encode (RTMP and MP4 from one encode, ADR 0001). |
| rayon pool | process | Data-parallel per-pixel kernels (tracker pyramid, compositor) *inside* a tick; holds no state. |

### Mailboxes and channels

| From → to | Mechanism | Policy |
|---|---|---|
| source → engine | `ac_media::LatestFrame` | Newest frame only. A frame overwritten before the engine read it is never queued; the engine counts the sequence gap as `missed`. |
| source audio → engine | bounded queue | Drained every tick, paced against video by `AudioPacer` (silence when audio lags more than 100 ms, drops when it leads more than 500 ms). |
| engine → discovery worker | `mailbox::Slot` (capacity 1) | At most one request in flight per source; a new request replaces an unclaimed one. The worker always works on the newest frame it was given. |
| discovery worker → engine | `mpsc` channel | One result per request (bounded in practice by the in-flight rule). Results are applied only if not stale (below). |
| operator → engine | `mpsc` channel of commands, each with an optional reply sender | Applied in order at the start of the next tick, so a command never lands mid-tick. Replies say done, or exactly why not. |
| engine → operator | `mailbox::Latest<Arc<EngineSnapshot>>` | Newest snapshot only, with a version counter; readers never block the engine. |
| engine → encoder | `ProgramEncoder` bounded queues | Never blocks: a full queue drops the frame and the writer repeats the last one, keeping a constant frame rate. |
| output helpers → engine | `mpsc` channel | Start and finish results, collected by `OutputController::poll` each tick. |

Nothing between threads is an unbounded queue of frames, and no lock is
held across I/O. The only locks are inside the mailboxes themselves, held
for a pointer swap.

### Clocks

Two clocks, never mixed:

- The **media clock** of each source is its frames' PTS. Tracking,
  subject identity and staleness run on it: a detection result is dropped
  as *superseded* when a newer frame's result was already applied, and as
  *too old* when its frame's PTS is more than `discovery.max_result_age_s`
  behind the source's newest frame. A subject's track observations count
  only while a detection has seen it within `discovery.max_undetected_s`
  (detections vouch for existence; the tracker alone cannot keep a subject
  alive).
- The **master clock** is the tick index times the output frame period.
  Framing steps, transitions, the director's dwell and hold times and the
  encoder's constant frame rate all run on it, and program audio is paced
  against the number of frames encoded.

The master clock is *paced* by an injectable `ac_sources::Clock`: the wall
clock in production, a virtual or sped-up clock in tests (sources are paced
by the same clock). `TickSchedule` marks a tick late when it starts more
than half a period after it was due and skips ticks rather than bursting
when the loop falls more than `max_lag` (1 s) behind. Core crates never read
a clock; `EngineCore::tick(now, feeds, results)` takes time as an argument
and is deterministic, which the core tests rely on (same inputs, identical
program frames).

### Ownership

- `EngineCore` (engine thread) owns per source the subject store and
  tracker (`SourceVision`), the query list, the shot catalog with one
  framer per shot, the switcher, the director and the compositor.
- `OutputController` (engine thread) owns the encoder session and its
  helper threads.
- `Runtime` (engine thread) owns the source handles and discovery workers.
- The UI owns only what the operator is typing and the T-bar lever position.
- Snapshots are immutable; their pictures (monitors and multiview tiles) are
  `Arc`-backed frames rendered at their own lower rates and shared between
  snapshots until re-rendered.

Shot ids are stable strings (`<source>/wide`, `<source>/<subject>/<preset>`),
so commands name shots, not indices. The subjects of the program and preview
shots are *protected*: they are not retired, and their shots stay listed
even after their query is removed or they are hidden, until they leave the
air.

### Shutdown

`Shutdown` (or dropping `Engine`, or a closed command channel) makes the
engine thread stop the discovery workers (joining them after a pass in
progress), shut the output down (finishing a running recording
synchronously, joining every helper, reaping ffmpeg), stop the sources
(joining their workers) and publish a final snapshot with `running = false`.
`Engine::shutdown` then joins the engine thread and reports the ticks run and
threads joined. A Linux test checks that no `ac-*` thread and no child
process remain.

## Consequences

- Latency is bounded and stale work is dropped instead of queued: under
  load the engine misses frames, skips ticks or drops detections, and each
  of those is counted in the snapshot and shown in the status bar.
- All per-pixel work for one output frame happens inside one tick on the
  engine thread plus the rayon pool, so the CPU compositor and tracker must
  fit the frame budget (about 6.5 ms per tick for the 720p demo in a debug
  build). A GPU compositor can replace `ac-compositor` behind the same call.
- Commands take effect at the next tick (at most one frame period later)
  and their effect is visible in the snapshot after that tick.
- Detection is asynchronous in real time even when the master clock is not,
  so a clock running much faster than the detector drops every result as
  too old; tests use a moderately sped-up clock, or drive `EngineCore`
  directly with inline discovery.
- One detector instance per source: sources do not share a model session.
