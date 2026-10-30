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
source, connection epoch, sequence number and PTS.

## Decision

`crates/ac-engine` runs the pipeline. All mutable studio state lives on
**one engine thread**; everything else talks to it through mailboxes and
channels, and the operator sees it only through immutable snapshots.

### Threads

| Thread | Owner | Job |
|---|---|---|
| UI (`main`) or headless runner | `apps/auto-camera` | Reads snapshots, posts commands. Never touches engine state. |
| `ac-engine` | `Engine` | The master clock loop: applies commands, runs one `EngineCore::tick` per output frame, pushes program to the encoder, publishes a snapshot. |
| source workers (`ac-synthetic-<id>`, file decoders, `ac-capture-<id>` with its `ffmpeg` child) | each `VideoSource` | Decode, generate or capture frames at the source rate; publish into the source's frame mailbox and a small bounded audio queue. A capture worker publishes each frame as it reads it (a camera runs in real time) and ends and reaps its ffmpeg before the connection stops. |
| `ac-capture-watch-<id>` | `CaptureSource`, one per connection | Waits on the connection's frame mailbox and cuts the connection (records why and kills its ffmpeg, which ends the worker, which records the error) when no first frame came within 10 s or no next one within 3 s; holds nothing but when it last saw a frame. |
| the `system_profiler` and `ffmpeg -list_devices` children (macOS), with an `ac-capture-list` reader per pipe | `SystemInventory`, on the thread that opens a capture source (the engine's start, then its `ac-supervise-<id>`) | List the connected cameras before every open and reconnect, so a unique ID is resolved to ffmpeg's current index; a tool that has not exited within 10 s is killed, and each is reaped before the open goes on. Nothing is cached. |
| `ac-supervise-<id>` | `SupervisedSource`, one per source | Owns the source's current connection and passes its frames (tagged with the connection's epoch) and audio on; when the connection fails, stops it, reaps its children and opens it again with a backoff of 0.5 s doubling to 8 s (below). |
| audio inputs: `ac-audio-supervise-<id>`, and under it `ac-audio-in-<id>` with its `ffmpeg` child (a file) or `ac-audio-dev-<id>` fed by the device's CoreAudio callback (a device) | each `SupervisedAudio`, owned by the engine's `ProgramAudio` | Decode or capture an audio-only input (`[[audio_inputs]]`), tag each block with its id, sequence number and PTS, and keep the blocks in a bounded `AudioRing` that drops its oldest; reopen a failed input with the same backoff. A device's callback only copies into a fixed pool of buffers for its worker, never waiting or allocating. See the `ac_sources::audio_input` module docs. |
| `ac-discovery-<source>` | `DiscoveryWorker`, one per source with a detector | Loads the detector (a CoreML compile can take minutes), then runs it on the newest frame it was handed. The OWL detector returns whole people, each tagged with the query whose object they hold (`ac_vision::matching`). |
| `ac-output-start`, `ac-output-finish` | `OutputController` | Start ffmpeg (probing encoders) and finish it (flushing the recording) off the engine thread, since both can take seconds. |
| `ac-output-video/-audio/-progress/-stderr` and the `ffmpeg` child | `ProgramEncoder` | Feed and watch the single program encode (RTMP and MP4 from one encode, ADR 0001). |
| `ac-trace`, with `--trace` | `apps/auto-camera` `TraceFile` | Writes the engine's per-tick trace records to the trace file as JSON Lines; nothing else writes to that file. |
| the `caffeinate` child (macOS) | `KeepAwake`, held by the `OutputController` while a session is live, or by the headless runner for the whole run | Keeps the system (and, when the monitors show program, the display) awake; killed and reaped when the session or run ends, and tied to the app by `-w`. |
| rayon pool | process | Data-parallel per-pixel kernels (tracker pyramid, compositor) *inside* a tick; holds no state. |

### Mailboxes and channels

| From → to | Mechanism | Policy |
|---|---|---|
| source → engine | `ac_media::LatestFrame` | Newest frame only. A frame overwritten before the engine read it is never queued; the engine counts the sequence gap as `missed`. The engine reads it whenever its thread wakes, and keeps what it read from every source but the phase reference in that source's `PresentationBuffer`, a ring of at most 4 frames, none older than the one shown last (below); a frame the buffer passes over counts as `missed` too. |
| source audio → engine | bounded queue | Drained every tick, paced against video by `AudioPacer` (silence when audio lags more than 100 ms, drops when it leads more than 100 ms). |
| engine → discovery worker | `mailbox::Slot` (capacity 1) | At most one request in flight per source; a new request replaces an unclaimed one. The worker always works on the newest frame it was given. |
| discovery worker → engine | `mpsc` channel | One result per request (bounded in practice by the in-flight rule). Results are applied only if not stale (below). |
| operator → engine | `mpsc` channel of commands, each with an optional reply sender | Applied in order at the start of the next tick, so a command never lands mid-tick. Replies say done, or exactly why not. |
| engine → operator | `mailbox::Latest<Arc<EngineSnapshot>>` | Newest snapshot only, with a version counter; readers never block the engine. |
| engine → encoder | `ProgramEncoder` bounded queues | Never blocks: a full queue drops the frame and the writer repeats the last one, keeping a constant frame rate. |
| output helpers → engine | `mpsc` channel | Start and finish results, collected by `OutputController::poll` each tick. |
| engine → trace writer | bounded `sync_channel` (`EngineOptions::trace`; the app's holds 1024 records) | Never waits: a full channel drops the record (the gap shows in the records' tick numbers), and a closed one ends tracing for the run. |

Nothing between threads is an unbounded queue of frames, and no lock is
held across I/O. The only locks are inside the mailboxes themselves, held
for a pointer swap.

### Clocks

Two clocks, never mixed:

- The **media clock** of each source is its frames' PTS. Tracking,
  subject identity and staleness run on it: a detection result is dropped
  as *old epoch* when its frame is from an earlier connection than the
  source's newest frame, as *superseded* when a newer frame's result was
  already applied, and as *too old* when its frame's PTS is more than
  `discovery.max_result_age_s` behind the source's newest frame. A
  subject's track observations count only while a detection has seen it
  within `discovery.max_undetected_s` (detections vouch for existence; the
  tracker alone cannot keep a subject alive).
- The **master clock** is the tick index times the output frame period.
  Framing steps, transitions, the director's dwell and hold times and the
  encoder's constant frame rate all run on it, and program audio is paced
  against the number of frames encoded.

The master clock is *paced* by an injectable `ac_sources::Clock`: the wall
clock in production, a virtual or sped-up clock in tests (sources are paced
by the same clock). `TickSchedule` marks a tick late when it starts more
than half a period after it was due and skips ticks rather than bursting
when the loop falls more than `max_lag` (1 s) behind. A `PhaseLock` keeps
ticks midway between the frames of a source running at the tick rate:
when that source (the program audio source, else the first) skips a frame
at a tick, the schedule is delayed by half a period (at most once a
second). Without it a 30 fps source and a 30 Hz tick on the same clock
collide, and scheduler jitter makes the engine repeat and skip about a
third of the frames. The ticks can sit midway between one source's frames
only; every other source is aligned by its `PresentationBuffer` instead
(`ac_engine::presentation`). The engine offers it the source's newest
frame whenever its thread wakes (at most 10 ms apart while it waits) and
shows, at each tick, the held frame whose PTS is nearest a target that
moves with the tick's due time, one frame period behind the newest frame
that can have arrived (measured when the buffer locks). Arrival jitter then
no longer decides between two frames: a source at the tick rate shows one
frame per tick and a 29.97 fps source repeats exactly one frame per 1,001
ticks, at the cost of one to two frame periods of delay. The buffer's ring
of at most 4 frames, owned by the engine thread, is the one deliberate
queue of frames on their way in; it drops every frame older than the one
it shows, and a source's frame pool remembers enough buffers for it, so
frames are still never copied or allocated per frame. A source without a
new frame for 1 s is stale: its buffer stops presenting it, and the
snapshot shows it stale with the buffer's counts of new, repeated and
passed-over frames (`SourceView::presentation`). Core crates never read
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

### Sources that drop out

Every source runs under a supervisor (`ac_sources::supervisor`). A
connection that stops with an error (ffmpeg killed or crashed, a file not
there yet) makes the source `Offline`, with the error in its snapshot; after
the backoff it is `Reconnecting`, and `Running` again from its first frame,
so an input that comes back is on air at most 8 s later plus the time to
open it (spec C.3). Each connection is a new *epoch*, counted from 0; the
supervisor restamps frames so sequence numbers and PTS keep increasing
across connections, and the time away counts as elapsed media time. The
core only compares epochs: a source that is not delivering is unavailable,
and a detection about an older epoch's picture is dropped however recent
its PTS. A source that cannot be opened at start does not stop the engine;
it starts offline, described by its config (a file by the program size at
an unknown rate), and the other sources run meanwhile. Once it delivers,
the supervisor describes it as its input does: the core takes the size of
its first frame and the rate of that frame's connection, and an output
started from then on carries its audio.

### Shutdown

`Shutdown` (or dropping `Engine`, or a closed command channel) makes the
engine thread stop the discovery workers (joining them after a pass in
progress; a worker still loading its model is detached and exits when the
load ends), shut the output down (finishing a running recording
synchronously, joining every helper, reaping ffmpeg and `caffeinate`), stop
the sources (joining their workers) and publish a final snapshot with
`running = false`.
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
