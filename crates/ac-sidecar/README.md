# ac-sidecar

Runs a detector in a separate process (first SAM 3.1 through mlx-vlm in
Python) and exposes it as an ordinary `ac_vision::Detector`. Noah chose this
runtime: SAM 3.1 in a Python/MLX sidecar under Meta's SAM License. It is
retired only if an in-process ONNX build meets the R1 exit criteria in
`model-picker-design.md` §4.6. This is Phase 2, at studio level: one sidecar
answers every query on every source.

| Item | What it does |
|---|---|
| `protocol` | Pure codec for protocol v1: frames, `hello` / `detect` / `result` / `error`, strict validation |
| `prep` | Pure frame preparation: RGBA to RGB8, box-filtered so the longer side is at most `send_side` (default 1008 px); never enlarges |
| `SamHost` | Single owner of one sidecar process: spawn, pipe threads, deadlines, stale-reply dropping, kill/reap, restart backoff |
| `SidecarService` | Puts a `SamHost` on its own thread so every source's detector shares one process |
| `SidecarClient` | Cheap, non-owning handle to the service; makes detectors |
| `SidecarDetector` | The `Detector`: groups queries by phrase, applies per-query thresholds, marks people as whole people |
| `ac-fake-sidecar` | Test binary that speaks the protocol with no model and misbehaves on demand |

## Protocol v1

Both sides must match exactly. The Rust side is `src/protocol.rs`; the
Python side (`sidecars/sam3-mlx/server.py`) is written against this section.

**Transport.** The host spawns the sidecar with stdin and stdout as pipes.
Stdout carries only protocol frames. All logs go to stderr, which the host
keeps as recent lines. The sidecar exits cleanly when stdin reaches end of
input. The host never sends a second `detect` before the first is answered
or abandoned (one request in flight).

**Frame.**

```text
u32 LE header_len | header_len bytes of UTF-8 JSON (the header) | u32 LE payload_len | payload_len bytes
```

The header is at most 1 MiB and the payload at most 64 MiB. Any larger
length is a protocol error. Every message except `detect` carries an empty
payload in v1.

**Messages.** Every field shown is required; a field that may be null is
written as `null`, never left out. No other field may appear, including in
nested objects.

```jsonc
// child -> host, once, after the model has loaded (payload empty)
{"type":"hello","protocol":1,"model":"sam3.1-mlx","weights_sha256":null,
 "input_side":672,"capabilities":{"masks":false,"parts":false}}

// host -> child (payload: width*height*3 bytes, RGB8, row-major, top row first)
{"type":"detect","id":7,"source":"pulpit-cam","seq":41,"pts_ns":1366666666,
 "width":1008,"height":567,"format":"rgb8",
 "phrases":[{"text":"a person speaking","threshold":0.3}],"want_masks":false}

// child -> host (payload empty; masks are reserved for a later version)
{"type":"result","id":7,"source":"pulpit-cam","seq":41,"pts_ns":1366666666,
 "latency_ms":482.5,"detections":[{"phrase":0,"box":[0.25,0.1,0.2,0.6],"score":0.71}]}

// child -> host; "id": null means fatal, and the child exits after sending it
{"type":"error","id":7,"message":"out of memory"}
```

* `box` is `[x, y, w, h]`, normalized to `0..1` of the image sent, with the
  origin at the top-left. `phrase` indexes the request's `phrases`.
* A result holds only detections at or above their phrase's threshold.
* It echoes the request's `id`, `source`, `seq` and `pts_ns`. A reply that
  does not match the outstanding request is dropped and counted (`stale`).
* Value rules, checked on both encode and decode:
  * `protocol` is 1, checked first, so a newer sidecar is reported as one;
  * a SHA-256 is 64 hex digits;
  * sizes are positive;
  * thresholds lie in `0..=1`;
  * numbers are finite;
  * `want_masks` is false;
  * the payload length matches the header.

  The host clamps small range slips such as a box edge at 1.0000001.

## Deadlines and failures

| What | Default | On a miss |
|---|---|---|
| `hello` after spawn (`HostConfig::startup_timeout`) | 60 s (SAM 3.1 loads in 4.6 s, measured) | kill, reap, `Timeout` |
| One request, from when it reaches the writer (`DetectorConfig::request_timeout`) | 5 s (about 5x the slowest measured pass: 1.05 s for 9 phrases at 672 px) | kill, reap, `Timeout` |
| Exit after stdin closes at shutdown (`HostConfig::exit_grace`) | 2 s | kill the process group, reap |
| Restart after a failure (`restart_backoff`, `max_restart_backoff`) | 1 s, doubling per consecutive failure, capped at 30 s | requests are refused (`Unavailable`) until it passes; the next request respawns |

The process is ended, and the host becomes unavailable, after any of these:

* a missed deadline;
* the child exiting;
* a protocol error;
* a fatal `error`.

The reason it reports names the exit status and the child's last stderr
line. An `error` reply with the request's id fails only that request; the
child stays up. A result or an error reply resets the backoff. A `hello` does
not, so a sidecar that loads and then dies on every request still backs off.

The first start is different. `SidecarClient::detector` waits for the first
`hello`, and if that start fails, the failure comes back as the detector's
load error.

## Ownership and threads

```text
discovery thread ──detect──▶ SidecarDetector ──job──▶ ac-sam-host thread (owns SamHost)
  (one per source)            (SidecarClient)          │ owns the Child (own process group)
                                                        ├─ ac-sam-write: owns stdin  ◀─ bounded(1)
                                                        ├─ ac-sam-read:  owns stdout ─▶ bounded(4) events
                                                        └─ ac-sam-log:   owns stderr ─▶ log ring
```

* **Single owner.** `SamHost::detect` takes `&mut self`, so the
  one-in-flight rule is enforced by the type system. The service thread is
  the host's only user.
* **Message passing.** Detectors send jobs over a channel and block on their
  reply. Each detector has at most one job queued, so the queue is bounded
  by the number of sources and served oldest first.
* **No locks across I/O.** The log ring is the only shared structure; its
  lock is held only to push or copy lines.
* **Stopping.** `StopHandle::stop` interrupts a wait in progress. Dropping
  `SidecarService` uses it, so quitting never waits out a 60 s startup.
* **Process group.** The child is spawned with `process_group(0)` (safe,
  no `unsafe`). Killing it kills the whole group, which takes helper
  processes it started too. After a clean exit the group is signalled as
  well, so no helper outlives the host.

## Using it (for the engine integration)

```rust
use ac_sidecar::{DetectorConfig, HostConfig, SidecarService};

// Once per studio, at engine start: spawns the sidecar (a bad command fails here).
let service = SidecarService::start(HostConfig::new([
    "../models/sam3/.venv/bin/python", "../sidecars/sam3-mlx/server.py", "--side", "672",
]))?;
// In each source's DetectorLoader (runs on its discovery thread): waits for hello.
let client = service.client();
let loader: DetectorLoader = Box::new(move || {
    let detector = client.detector(DetectorConfig::default()).map_err(|e| e.to_string())?;
    Ok(Box::new(detector) as Box<dyn Detector>)
});
// At shutdown, after the discovery workers stopped: closes stdin, 2 s grace, kill, reap.
drop(service);
```

The service, not the detectors, owns the process. Keep the
`SidecarService` in the engine and drop it after the workers. Detectors that
outlive it, and loaders still waiting for `hello`, get `Unavailable`.

Settings an integrator exposes:

| Setting | Type | Default | Notes |
|---|---|---|---|
| `command` | `[String]` | — (required) | Program and arguments; resolve relative paths against the config file |
| `current_dir` | path, optional | host's | Working directory of the child |
| `startup_timeout_s` | seconds | 60 | Wait for `hello` |
| `request_timeout_s` | seconds | 5 | Keep it below `discovery.timeout_s`; with N sources sharing, a request may also queue behind N-1 others |
| `threshold` | `0..=1` | 0.5 | Default score; the example config uses 0.3 for the speaker |
| per-query `threshold` | `0..=1` | — | `DetectorConfig::thresholds`, keyed by `QueryId` |
| `send_side` | px, `1..=4096` | 1008 | Longest side of the image sent |
| `exit_grace_s`, `restart_backoff_s`, `max_restart_backoff_s`, `log_lines` | | 2, 1, 30, 200 | Rarely changed |

`DetectorConfig::lexicon` should be the studio's prompt lexicon
(`StudioConfig::prompt_lexicon()`), so person words added in the config also
mark sidecar boxes as whole people.

## Status

`SidecarService::status()` returns a `HostStatus` snapshot, published after
every job:

* the state: `Idle`, `Starting`, `Ready`, `Unavailable` (with the reason and
  the retry time) or `Stopped`;
* the child's pid and `hello`;
* counters: spawns, failures, requests, results, rejected, timeouts, stale,
  refused, protocol errors, detached threads;
* the last pass latency and round trip.

`logs()` returns the recent stderr lines, with the host's own `[host]` notes
on starts, `hello`s and exits. Phase 2's exit criterion of "0 sidecar
restarts" in a 30-minute run reads as `failures == 0` (equivalently
`spawns == 1`).

## Tests

```sh
cargo test -p ac-sidecar
```

No test needs a GPU, a camera, the network or Python. The integration tests
drive the host, the service and the detector through every mode of
`ac-fake-sidecar`: `ok`, `slow`, `hang`, `crash-after-hello`,
`crash-before-hello`, `crash-once`, `garbage`, `wrong-seq`, `never-ready`,
`error-reply`, `fatal-error`, and `--grandchild` for helper processes (see
`src/bin/ac-fake-sidecar.rs`).

* Every test checks that its child was reaped.
* `tests/leftovers.rs`, a single-test binary, then checks that this process
  has no child process left and, on Linux, no `ac-sam-*` thread.
* The codec tests decode the headers above verbatim, cut a stream at every
  byte, and fuzz frames and headers with a seeded generator.

## Not here

These are later phases (design §5):

* masks, parts and per-query model choice;
* lanes and paired passes with OWLv2;
* TCP to a GPU node;
* in-process ONNX SAM;
* the SAM video tracker.

The Python `server.py` lives outside the Cargo workspace.
