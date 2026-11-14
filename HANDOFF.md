# Handoff (temporary)

Updated 2026-11-01. Read `CLAUDE.md` first. The previous handoff's camera,
CoreAudio timestamp and drift-correction fixes are merged. Physical-board
validation remains open. Delete this temporary file when its information has
been absorbed into the normal project records.

## Project and standing rules

- Work in `/Users/nover/repos/auto-camera` on Noah's Apple Silicon Mac.
  Never modify the old prototype at `/Users/nover/Documents/ChatGPT/Auto camera`.
- The external specification is `research/lay-of-the-land.md`; architecture
  decisions are in `docs/adr/`. The [README](README.md) describes the product.
- GitHub Actions is unavailable because of billing. Do not rerun Actions,
  set up a runner, or repeatedly comment about CI.
- Run the manual gates below on the exact PR head before merging, with a
  merge commit. Never force-push or rewrite branch history.
- Leave no app, ffmpeg, sidecar or test processes running.
- Noah's unrelated `platonic` computation takes priority. Do not pause or
  otherwise change it to make timing checks pass.

## Software changes

- [Camera PR #32](https://github.com/NoahIsCool/auto-camera/pull/32) releases
  every completed rawvideo frame promptly. FIFO test writers can stop during
  open or write. Decoder reaping retains ownership until the child exits;
  log-session locks are explicitly released after the buffered writer closes.
- [CoreAudio PR #29](https://github.com/NoahIsCool/auto-camera/pull/29) selects
  device inputs by exact UID, reconnects without substituting another
  microphone, and reports rate mismatches and discontinuities. The Stamper
  now uses a robust recent phase reference and prior-noise thresholds. It
  preserves noisy real rewinds, avoids cumulative movement from jitter and
  ramps, and treats split first callbacks correctly.
- [Drift PR #30](https://github.com/NoahIsCool/auto-camera/pull/30) corrects
  independent device clocks and protects a confirmed step's level while its
  fit settles, including returning excursions and variable blocks crossing
  that interval. It includes the current late-tick pacing fixes and the
  completed end-to-end scenario battery.
- The engine's phase lock now compares consecutive, timely reference reads.
  After an engine stall, it resumes on its original tick grid. The 30 fps
  regression covers both skipped ticks and a burst of catch-up ticks.

The implementation and regression commits include `bbdf1c9` (camera release
and shutdown ownership), `0564dfa` (Stamper), `4171104` plus `d5c7557` (drift
guard), and `4c4f0cc` plus `e453e31` (battery). The PR bodies record final
integration validation and the remaining board checks.

The full release battery passes 12,030 identifiable scenarios and 24 separate
startup-ambiguity policy cases. The latter have identical observable inputs
for competing histories and are not exact-loss reconstructions. Assertions
also measure raw PTS movement, final audio displacement and net inserted/cut
frames independently of the stamped timeline. Baselines were run against both
older Stamper implementations; neither passed the full battery.

Reproduce the wide battery:

```sh
AC_STAMPER_SWEEP=full cargo test --locked --release -p ac-engine --lib battery -- --nocapture
```

Changed-module branch coverage measured separately: Stamper timing 59/60,
excursion 14/14 and anchor 13/14; drift estimator 45/48 and epoch 29/32.
These are module results, not full-project coverage. Verus was unavailable;
no formal verification is claimed. Native CoreAudio compilation and synthetic
recording checks do not establish physical-board behavior.

## Manual gates

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
ORT_SKIP_DOWNLOAD=1 cargo clippy --locked --workspace --features auto-camera/onnx --all-targets -- -D warnings
cargo clippy --locked --target aarch64-apple-darwin --workspace --features auto-camera/coreaudio,ac-sources/coreaudio-hw --all-targets -- -D warnings
AC_REQUIRE_FFMPEG=1 cargo test --locked --workspace
cargo +1.92 check --locked --workspace --all-targets
python3 -B scripts/module_size.py
python3 -B scripts/licence_manifest.py
python3 -B -m unittest discover -s scripts -p "test_*.py"
```

When sidecar files change, also run their unit tests and Ruff checks. Known
load-sensitive tests include the restarted-receiver stream test, real-time
loudness test and camera alignment checks. Investigate an idle rerun and an
unchanged-main comparison before declaring a regression; do not weaken their
assertions to accommodate contention.

## Roadmap record

Keep roadmap information in this file, as Noah requested on 2026-10-30.
The original private roadmap is the
[Claude artifact](https://claude.ai/artifact/V6Cx7mh64gG6AvphP6aecs).
Its contents and current row statuses could not be read: it required sign-in
and a CAPTCHA. After the verified merges, Noah explicitly said **"Leave the
roadmap for later."** We did not edit the external artifact; its update is
deferred. The local statuses below record completed software, not a claim
that the Claude rows were updated or physical hardware was validated.

| Package | Local software status | Merged work | Remaining validation |
|---|---|---|---|
| A6 — CoreAudio board input | Done | [PR #29](https://github.com/NoahIsCool/auto-camera/pull/29), gated head `fbdd18b`, merge `feea725` | Board UID/channel/rate listing; physical capture; unchanged sample rate; readiness; unplug/replug without default-microphone substitution. |
| A5 — Audio clock-drift correction | Done | [PR #30](https://github.com/NoahIsCool/auto-camera/pull/30), gated head `d4a34ea`, merge `db4658a` | Hour-long clap/alignment check within 20 ms; correction/reset/step telemetry; reconnect recovery; rate-change warning and output restart. |

Camera capture and shutdown fixes also merged in
[PR #32](https://github.com/NoahIsCool/auto-camera/pull/32), gated head
`bbdf1c9`, merge `1152553`. Final integrated `main` at `db4658a` has the same
source tree as the gated A5 head: all required manual gates passed, with
1,964 Rust tests/doc-tests, 42 script tests, the 12,030 identifiable scenarios
and 24 separate ambiguity-policy cases above. Native CoreAudio builds and
synthetic recording smoke tests passed; the final recording had zero drops.
The PR checklists retain the exact physical-board commands and acceptance
steps. Detailed local evidence is in
`scratch/handoff-2026-10-30/RESULTS.md` (ignored scratch, not tracked in Git).

When the external update is resumed, A6 and A5 should be marked Done for the
merged software while retaining the outstanding hardware checks. No other
roadmap package statuses were read or inferred. The known follow-ups and
decisions waiting on Noah remain listed below.

## Next work

1. Noah: complete #29's physical-board checklist, then #30's hour-long A/V
   alignment, reconnect and sample-rate-change checks.
2. Keep the local roadmap record above current. Leave the external Claude
   update deferred until Noah resumes it.
3. Complete the round-3 Mac checks for framing PR #6. Its checklist remains
   `git show origin/notes/mac-checks:mac-checks.md`; leave that draft pending
   Noah's footage run.

## Known follow-ups

- After a fresh source restart, pacer recovery can cut a late tick's audio,
  leaving it early by up to about 80 ms.
- Strict rustdoc still fails on pre-existing links to private items in
  ac-core prompt and ac-engine core/show; capture/inventory links were also
  recorded in the original handoff. Public and private-item documentation
  builds with warnings denied need cleanup.
- Measure `-thread_type slice` throughput on the Mac's ffmpeg; the older
  ffmpeg 6.1 measurement showed a 4K throughput cost of about 20%.
- Timestamp classification remains heuristic. Events inside the learned
  noise envelope and indistinguishable startup histories cannot always be
  recovered exactly.

Still waiting on Noah: evaluation footage, cameras/streaming platform/priority
subjects, ADR 0003's open framing and zoom decisions, and a spec amendment
before raw multitrack audio work.
