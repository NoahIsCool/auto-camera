# Mac checks, 2026-10-24

From the cloud thread session (the one you report to by cross-session message). Work in
`~/repos/auto-camera`. The old prototype folder (`/Users/nover/Documents/ChatGPT/Auto camera`)
stays read-only: read its footage and weights in place, never write there. Report numbers;
do not push to any branch. Leave no app, ffmpeg or Python process running when done.

## 0. Update

`git fetch origin`, then check `git log origin/<branch>..<branch>` is empty for every local
branch (stop and report if not). Branches: `prompt-matching` (PR #3, 96d9d57), `shot-words`
(PR #4, e0f8766, on top of #3), `sam-sidecar` (1ed3564, on top of #4, still being wired up).

## 1. SAM 3.1 sidecar smoke test (branch sam-sidecar, Python only)

Follow `sidecars/sam3-mlx/README.md`, "On Noah's Mac":
- symlink the old folder's `models/sam31-mlx` to `~/repos/auto-camera/models/sam31-mlx`;
- check the venv's versions (mlx 0.32.3, mlx-vlm 5b22c24, transformers 5.18.0);
- `python3 -m unittest -v` in `sidecars/sam3-mlx` (fake backend, no MLX);
- `tests.test_mlx_smoke` with `AC_SAM3_MODEL` set and `AC_SAM3_IMAGE` set to a frame you
  extract from the sermon clip.
Then, in `scratch/sidecar/`, a small client (reuse `tests/support.py`) that sends 5 frames
each from the sermon and band clips with phrases "a person speaking", "a person playing
guitar", "a person" at threshold 0.3, and prints time to hello, per-request latency and the
boxes. Compare with your earlier `scratch/sam3/` results on the same frames (box IoU, counts,
timings: 0.48 / 0.64 / 1.05 s for 1 / 3 / 9 prompts at 672 px). Report any exception verbatim.
If something in server.py is wrong, describe the fix; do not commit it.

## 2. PR #3 re-check (branch prompt-matching, Rust, OWLv2 on CoreML)

Same 40 labelled frames (`scratch/pm/gt.json`), found / false per prompt, as before:
"a person", "a woman", "a person speaking", "a person playing guitar",
"a person playing drums", "the drummer", "a person in front of the screen". The last is a
risk check: an object now belongs to the person sharing most of the smaller box, so photos
of people inside the screen box could win over a real person in front of it.

## 3. Group-rule variants (Python on the same raw OWLv2 "a person" boxes)

Against the current rule (after NMS, drop a box holding >= 80% of two or more other boxes):
- A: current rule;
- B: a box is a group only if its score <= the best score among the boxes inside it;
- C: decode without NMS, drop groups, then run NMS;
- D: like A, but inner boxes must be distinct (pairwise IoU < 0.5) and each under 60% of the
  outer box's area, after removing nested part boxes (a lower-scored box >= 80% inside a
  higher-scored one with under 40% of its area).
For each: real people found, false boxes, real people wrongly dropped, and the frames where
variants differ.

## 4. PR #4 framing on the footage (branch shot-words)

Headless, 60 s each on the sermon and band clips (OWLv2 people-first, CoreML), queries
"speaker upper body", "guitarist close-up", "drummer wide". Save one image per shot every
10 s to `scratch/shots/` (a contact sheet), and report from the headless summary: the shots
made, how many are marked limited_by_zoom, and whether Close looks like head and shoulders
where it is not limited. Screenshot the operator window if the screen is unlocked.

## Report

Write everything to `scratch/mac-checks-2026-10-24.md` and send the cloud thread session a
short summary by cross-session message.
