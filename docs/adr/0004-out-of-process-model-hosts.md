# ADR 0004: Out-of-process model hosts

Status: Accepted (2026-10-15). Noah chose "Sidecar now" on a decision card
on 2026-10-15: run SAM 3.1 in a Python/MLX sidecar on their Mac, under
Meta's SAM License, retired only if an in-process ONNX build meets the R1
exit criteria below.

## Context

SAM 3.1 finds people by what they do, which OWLv2 cannot express: "a
person speaking" found the sermon speaker in 10 of 20 labelled frames at a
0.5 threshold and 14 of 20 at 0.3, with no false subjects; OWLv2 found that
speaker in 1 of 20 (measured on Noah's footage; `model-picker-design.md`
§1, §2, `sam31-vs-owlv2.md`). Noah chose SAM 3 for its arbitrary text
prompts (ADR 0003).

The only SAM 3.x runtime ever measured on Noah's M3 Max is mlx-vlm, in
Python: 0.48 s per pass for 1 prompt, 0.64 s for 3 and 1.05 s for 9 at
672 px, a 4.6 s load and a 3.3 GiB peak (measured). No SAM 3.x result on
ONNX Runtime's CoreML execution provider was found, and the CoreML
MLProgram op table lacks ops the community graphs use (design §4.6).

CLAUDE.md records the decision that "vision models run through ONNX Runtime
behind traits, Burn ports later". Running SAM 3.1 today means running it
outside ONNX Runtime and outside the Rust process.

## Decision

**A detector may run in a separate process, a sidecar, as an executor
behind the same `ac_vision::Detector` trait, not as a second architecture.**
Nothing downstream of the detector (subjects, framing, switching, output)
knows where a model runs.

- **Protocol v1** (`crates/ac-sidecar/README.md`): length-prefixed JSON
  headers with a binary RGB payload over the child's stdin and stdout;
  `hello`, `detect`, `result` and `error`; strict validation on both sides.
  The child is stateless between requests. Every request and answer carries
  the frame's source id, sequence number and PTS, and a reply that does not
  match the outstanding request is dropped and counted. The protocol does
  not depend on its transport, so it can later run over TCP to a GPU node.
- **Deadlines and restarts.** 60 s for the hello, 5 s per request once the
  child has it, 2 s to exit after stdin closes. A missed deadline, an exit,
  a protocol error or a fatal error kills and reaps the process; requests
  are refused while a backoff of 1 s, doubling up to 30 s, runs, and the
  next request starts a new process. That includes the first start: a
  load that misses its deadline or crashes does not turn detection off for
  the session; the passes fail with the reason until a restart says hello.
- **No orphan processes.** The child is spawned in its own process group
  (safely, without `unsafe`), and the whole group is killed and reaped. The
  engine owns the host and drops it at shutdown before it joins the
  discovery workers, which interrupts a pass or a load in progress, so
  quitting does not wait out a request deadline. The app handles Ctrl-C,
  `SIGTERM` and `SIGHUP` by running that shutdown (a second signal exits
  at once). Because the group no longer gets the terminal's Ctrl-C, the
  sidecar also watches its parent: once the host is gone (killed, crashed)
  it exits with status 5 within a fraction of a second, which covers macOS,
  where there is no parent-death signal. Tests assert that no child process
  is left on every Unix, and that no `ac-*` thread is left on Linux, where
  threads can be listed from `/proc` (`crates/ac-engine/tests/shutdown.rs`,
  `crates/ac-sidecar/tests/leftovers.rs`); the engine test also checks that
  shutdown with a pass hung in the sidecar takes under 4 s.
- **One process per studio.** SAM 3.1 needs about 3.3 GiB and saturates the
  GPU, so every source's detector is a client of one shared process. This
  amends ADR 0002's "one detector instance per source": each source still
  has its own detector instance on its own discovery thread, but the model
  behind them runs once. A queue in front of that process would break the
  latest-frame rule (with N sources each frame would wait about N − 1
  passes, and two sources at 1.05 s a pass already exceed the 2 s result
  age limit, so every answer would be thrown away), so the engine keeps
  **one request in flight for the whole studio** when its detector is
  shared, and gives the turn to the source that has waited longest, with
  its newest frame. The turns start once the process has said its first
  hello: until then the shared detector is preparing for every source, so
  nothing is sent and no timeout is counted. Answers are then about one pass old whatever the
  number of sources, and each source gets a pass about every N × the pass
  time. A frame that still waited longer than `max_result_age_s` for the
  process (behind a restart, or behind a request the engine gave up on) is
  skipped without a pass.
- **Studio level first** (design §5, Phase 2): `[detector] kind =
  "sidecar"` makes the sidecar the one detector of the studio, and every
  query goes to it (`config/sam3-sidecar.example.toml`). SAM 3.1 sees
  actions, so a role such as "speaker" draws no notice to take its shot by
  hand on the sidecar, as it does on OWLv2 (ADR 0003;
  `DetectorSettings::sees_actions`). Per-query models, lanes and paired
  passes with OWLv2 are Phase 3.
- **The first sidecar** is `sidecars/sam3-mlx/server.py`: SAM 3.1 image
  detection through mlx-vlm at revision `5b22c24`, boxes only, no masks,
  parts or video tracker. Its fake backend needs only the Python standard
  library and runs in CI, against its own tests and against the Rust host
  (`crates/ac-sidecar/tests/python.rs`).

### This qualifies "vision models run through ONNX Runtime"

The ONNX Runtime decision stays the rule for models that run in-process
(OWLv2 today), and the place for in-process SAM later. The sidecar is the
exception for SAM 3.1 on the Mac, rung R0 of the design's runtime ladder
(§4.6), until rung R1 meets its exit criteria:

> **R1**: in-process `ort` + CoreML, three graphs from community exports. A
> 3-day spike (B8): all graphs compile with static shapes;
> `ProfileComputePlan` shows the encoder on the GPU or ANE; latency ≤ 1.2×
> R0; box IoU ≥ 0.9 on the labelled frames. If it passes, the sidecar is
> retired on the Mac.

Phase 5 of the design then removes it: parity tolerances met (the same
instances above threshold, mask IoU ≥ 0.95, score difference ≤ 0.02),
latency ≤ 1.2× the sidecar, and Milestone 2 passing again without it. If
B8 fails, the sidecar stays. CLAUDE.md's decision line is Noah's to update.

## Consequences

- Python enters the repository, outside the Cargo workspace, in
  `sidecars/`. CI runs its tests (`python3 -m unittest`) and the Rust tests
  that drive `server.py --backend fake`; anything that imports MLX runs
  only on a Mac (`tests.test_mlx_smoke`, by hand).
- **Licence.** The SAM 3.1 weights are Meta "SAM Materials" under the SAM
  License (`sidecars/sam3-mlx/SAM_LICENSE.txt`). They are not in the
  repository. A release bundle that includes them must carry the licence;
  its prohibited uses (ITAR, military and weapons, nuclear, espionage) and
  its termination clause apply to anyone using them.
- **Operation.** A sidecar whose program or paths are missing, or that
  cannot be spawned, stops the studio from starting, with the reason. One
  that dies before its hello (a wrong `--model` sends a fatal error in
  milliseconds) or misses its startup deadline makes every discovery pass
  fail with that reason while the host retries under its backoff, so a
  transient failure heals itself and a permanent one costs one short
  attempt every 30 s. While the first start loads the model, the window
  says "preparing the detector" (dimmed, not amber) and the status bar
  how long it has been preparing; a headless run waits for it, and its
  summary says whether each source's detector was ready and when. The
  window marks a source whose newest pass failed, went stale or timed out
  in amber, with the reason on hover, and counts errors, stale results and
  timeouts in the status bar. Sources, tracking,
  switching and output keep running whatever the sidecar does.
- **Scores.** SAM and OWLv2 score on different scales, so the sidecar has
  its own `score_threshold` (0.5 by default), and the subject store's floor
  follows it when it is lower than the store's 0.35.
- **Not yet built** (design §4.6 and §5): checking the weights' SHA-256
  against an expected value, the bundled-image self-check in `hello`, a uv
  lock file, most of the sidecar's state in the engine snapshot and the
  headless summary: only its first start is there, as each source's
  `preparing` and ready time (`discovery_preparing`, `detector_ready_s`);
  its restarts, its process and what its hello named are not
  (`ac_sidecar::HostStatus` has them, but Phase 2's "0 sidecar restarts"
  can only be inferred from discovery errors and timeouts today),
  capabilities declared in the hello (the engine takes every sidecar to
  see actions, from its kind), per-query thresholds, and TCP.
