# Auto Camera development rules

Product: AI camera crew for live streams. Text prompts select subjects on each
video source; every matched subject gets smoothly framed virtual shots; an
operator (optionally helped by an auto-director) switches shots in a
preview/program setup; program goes out as RTMP and a local MP4 recording.
Experimental: AI scoring/balancing of raw multitrack audio from the soundboard.

Decisions (see docs/adr/): Mac first (Apple Silicon) with a portable core;
vision models run through ONNX Runtime behind traits, Burn ports later;
outputs are RTMP + MP4 from one encode; the operator UI is a native window;
autonomy is operator A/B plus suggestions plus opt-in auto-take.

## Standards

- Rust 2024, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test` must all pass before any commit.
- Every public and private item has `///` docs; each module has `//!` docs
  explaining the theory and invariants, not just what the code does.
- Core crates are pure and deterministic: no I/O, no wall clocks, time enters
  as `dt` or as media timestamps. I/O lives in adapter crates behind traits.
- One owner per piece of state. Prefer message passing (channels) and
  immutable snapshots over shared locks. Never hold a lock across I/O.
- Real-time rules: latest-frame mailboxes (never queue unbounded frames),
  drop stale results, tag every frame and observation with source id,
  sequence number and PTS.
- Tests: unit tests next to the code; cover normal behaviour, invalid input
  and edge cases. Hardware/model tests are behind cargo features and skipped
  when artifacts are absent. No test may need a GPU, camera or network.
- Keep crates small; a module over ~600 lines is a smell. No scope creep:
  every feature must trace to the spec in /mnt/project-files/research/lay-of-the-land.md.
- Never leave servers, encoders or child processes running after a task.
