# Handoff (temporary)

This file is for an agent picking up the project while the usual one
is paused until Noah's usage resets (written 2026-10-30). Delete it
when work comes back. Read `CLAUDE.md` first; its rules still apply.

## The project

Auto Camera is an AI camera crew for live streams, written in Rust; the
[README](README.md) has the overview, and the crates are listed under
"Crates". The spec and the original survey live outside the repo, in
the project's shared files (`research/lay-of-the-land.md`). The design
decisions are in [`docs/adr/`](docs/adr). The roadmap to production,
with a status per work package, is the Claude Doc
https://claude.ai/artifact/V6Cx7mh64gG6AvphP6aecs.

## Where it runs

- Development is on Noah's Mac (Apple Silicon), in
  `/Users/nover/repos/auto-camera`. It needs ffmpeg from Homebrew. The
  workspace's oldest supported Rust is 1.92.
- Never modify `/Users/nover/Documents/ChatGPT/Auto camera`. That is the
  old proof of concept, kept for reference only.

## Noah's standing rules

- CI is off. GitHub Actions cannot start jobs (billing), so every check
  shows red at once. Do not re-run the checks or set up a runner, and
  do not comment more than once per PR about it.
- Run the gates below by hand on the exact commit you merge. A PR that
  passes them may be merged, with a merge commit.
- Never force-push or rewrite a branch's history. Leave no ffmpeg, app
  or test process running.

## Gates

Run all of these from the repo root:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
ORT_SKIP_DOWNLOAD=1 cargo clippy --locked --workspace --features auto-camera/onnx --all-targets -- -D warnings
# only once ac-sources has the coreaudio feature (PR #29):
cargo clippy --locked --target aarch64-apple-darwin --workspace --features auto-camera/coreaudio,ac-sources/coreaudio-hw --all-targets -- -D warnings
AC_REQUIRE_FFMPEG=1 cargo test --locked --workspace
cargo +1.92 check --locked --workspace --all-targets
python3 -B scripts/module_size.py && python3 -B scripts/licence_manifest.py
python3 -B -m unittest discover -s scripts -p "test_*.py"
# when sidecars/ changes: its unittests and ruff, as in .github/workflows/ci.yml
```

Two tests are known to fail now and then under load: ac-output's
restarted-receiver stream test and ac-engine's real-time loudness test.
Rerun one of those on an idle machine before calling it a regression.

## State on 2026-10-30

`main` is at 11c66c2: PRs #1 to #28 are merged, with 1760 tests.

| Branch | Head | PR | State |
|---|---|---|---|
| `capture-stall-test` | 1c696fb | none yet | Gated and ready to merge. The camera source releases each frame at once (`-thread_type slice`), which fixes the flaky stall test. |
| `a5-drift-resampling` | b1377c8 | #30 (draft) | Drift correction and the pacer fixes. Gated on its own. Merge after #29. |
| `a6-coreaudio-input` | 94fc22f | #29 (draft) | CoreAudio board input. **Not mergeable**, see below. |
| `merge-train-5` | 2bcf60e | none | Rehearsal of main + #29 + #30 as of an older round. It lacks 94fc22f, 17261d6 and b1377c8. |
| `stamper-battery` | 169a7ad | none | Work in progress, not gated, may not compile. It holds a scenario battery for #29's Stamper. |
| `round2-fixes` | da1ce69 | #6 (draft) | Framing fixes. Waits on Noah's Mac run; the checklist is `git show origin/notes/mac-checks:mac-checks.md`. |

### Why #29 is blocked

The Stamper in `crates/ac-sources/src/audio_input/device/timing.rs` and
`timing/excursion.rs` turns each block's host time into a PTS. Its
latest version, 36faef2, has two confirmed medium regressions:

1. **A lasting step back is cut.** A real, lasting step back of the
   timestamps, under jitter above about ±0.15 ms, is taken for a glitch.
   The map is put back and the drift correction cuts that much audio, so
   the board plays 2.5 to 30 ms early for good. The cause is
   `LEVEL = MIN_TOLERANCE/4` in excursion.rs, a fixed 0.25 ms steadiness
   test.
2. **The map walks.** Bursts of jitter over 1 ms, or timestamps that
   creep late and snap back, move the map ahead for good. Lip sync then
   slides later by tens of ms within minutes. The cause is that an
   excursion is judged against the one block before its jump, not a
   stable timeline.

There is also one low finding. With IO buffers over 1024 frames, the
first buffer's error is not provisional. The fix is
`let second = self.second && !captured.cut;` before the first-excursion
check.

The version before, 1df4eaf, had neither regression, but it had the
problems that 36faef2's commit message lists (F2 to F5).

## Next steps

1. **Merge the camera fix.** Open a PR for `capture-stall-test`, run
   the gates and merge it. It is independent of everything else.
2. **Finish the battery.** Make it compile and commit it. It has
   families M1 to M10, which must pass, and S1 to S3, which should. Set
   `AC_STAMPER_SWEEP=full` for the wide grid in release mode.
3. **Get baselines.** Score the battery against 36faef2 and against
   1df4eaf's `timing.rs`.
4. **Fix the Stamper until no must family fails.** There are two
   candidate designs:
   - judge blocks against a slow robust anchor, such as the median over
     1 to 2 s of calm blocks, with jitter-scaled thresholds;
   - or keep 36faef2 and add a "noisy stream" flag, a confirmed-timeline
     flag and a jitter-scaled `LEVEL`.

   Put the Stamper change in one commit on `a6-coreaudio-input`.
5. **Re-merge the rehearsal.** In `merge-train-5`, merge
   `a6-coreaudio-input`, then `a5-drift-resampling`, then
   `stamper-battery`. #30 added an argument to the pacer's `plan`, so
   add `, Duration::ZERO` to its calls at
   `crates/ac-engine/src/program_audio/drift/tests/device.rs:363` and
   `program_audio/rate_tests.rs:189`. Run the gates on the result.
6. **Merge #29.** Its head contains main, so merge it once its head is
   gated.
7. **Merge #30.** Merge the new main into `a5-drift-resampling`,
   resolving conflicts as the rehearsal did; the trees should match.
   Then gate it and merge it.
8. **Hand over to Noah.** Set the roadmap rows A6 and A5 to Done. Ask
   Noah to run the board checks in #29's PR body on the Mac with the
   board plugged in.

## Known follow-ups, not started

- After an engine stall, PhaseLock adds half a period of lag (16.7 ms
  at 30 fps) for good.
- After a fresh source restart, the pacer's recovery cuts a late
  tick's audio, leaving audio early by up to about 80 ms.
- `RUSTDOCFLAGS=-D warnings cargo doc` fails on old links in
  ac-sources (`capture.rs`, `capture/inventory.rs`) and ac-engine
  (`core.rs`, `show.rs`).
- On ffmpeg 6.1, `-thread_type slice` is about 20% slower at 4K than
  ffmpeg's defaults. Measure the Mac's newer ffmpeg.

## Waiting on Noah

- The round-3 Mac run for #6.
- The footage-for-evaluation question.
- The cameras, streaming platform and priority subjects.
- ADR 0003's open items and the zoom limit.
- A spec amendment before any raw multitrack audio work.
