# Mac checks, round 3

From the cloud thread session (the one you report to by cross-session message). Work in
`~/repos/auto-camera`. The old prototype folder (`/Users/nover/Documents/ChatGPT/Auto camera`)
stays read-only: read its footage and weights in place, never write there. Report numbers;
do not push to any branch. Temporary logging goes in uncommitted edits that you revert at the
end (`git status` clean). Leave no app, ffmpeg or Python process running when done.

Round 2 is in `scratch/mac-checks-2026-10-24-round2.md` with its files under `scratch/r2/`.
PRs #1 to #5 are now merged into `main`. The fixes for round 2's three findings are on branch
`round2-fixes` (PR #6). What changed, and what each step checks:
- **A person cut off by the bottom of the picture** (the sermon speaker, box about
  (0.45, 0.65, 0.10, 0.35)) is now framed down from the crown on an assumed head of a third
  of the box (crown to waist), smoothed per shot, instead of the fill rule. Expected on the
  speaker: Close about 0.25 high and LIMITED, Medium close about 0.285, Medium about 0.38,
  crown headroom about 3 / 6 / 8%, Full unchanged at about 0.875. A box ending within 3% of
  the bottom keeps reading as cut until a whole, measurable box ends above that band. Step 1.
- **Alias shots need evidence**: a query found on a subject frames shots only once 2 detector
  passes over that subject have seen it, and stops after 8 such passes in a row without it
  (except shots already held while on program or preview). Identity is unchanged, so a
  subject can still list an alias that frames no shot. Expected: no Guitarist shot on an
  audience member from one stray guitar box. Step 2.
- **Tracker contrast check**: a weak match (score 0.5 to 0.8) whose window is more than 2.5x
  or less than 1/2.5 the template's contrast, and that lands more than a quarter of the box's
  shorter side from where the subject was heading, no longer moves the box; the tracker
  coasts. A tracker that only coasted for that reason since the subject's previous detection
  is restarted by the next detection, unless the match held back on that detection's frame
  overlapped the detection but no more than the subject's box did (a brighter lookalike
  drew it, and the subject stayed put). Expected: the shadow subject (round 2's subject 8)
  no longer rides the right guitarist's head. Steps 2 and 3.

## 0. Update and build

`git fetch origin`, then `git checkout -B round2-fixes origin/round2-fixes` (a new local
branch; nothing local is lost). Expected head: da1ce69 or later. Then
`cargo build --release -p auto-camera --features onnx`. Run everything below on this build.

## 1. Sermon framing (OWLv2)

The round 1 sermon OWLv2 config, 60 s headless, JSON to `scratch/r3/sermon.json`.
Report, from the summary, for every shot of the speaker subject: name, `crop_height_min/max`,
`crop_width_min/max`, `limited_by_zoom`, `same_picture_as`. Compare with round 2
(`scratch/r2/`): the Close was 0.373 to 0.382 and not limited.

Then add a temporary per-tick `eprintln!` in `crates/ac-engine/src/catalog.rs` where each
subject shot's framer is stepped, for the speaker's Close, Medium close and Medium only:
time, subject bbox, `ac_framing::person::looks_truncated(bbox)`, the framer's head memory
(`head_height()` and `assumed_heads_in_force()` on its `HeadMemory`, if reachable; otherwise
skip), the composed target crop height, the current crop height and `limited_by_zoom`. CSV to
`scratch/r3/sermon-close.csv`. Report: the share of ticks the box reads as cut by the bottom,
how often `limited_by_zoom` flips on the Close, the range of each shot's target crop height
over the last 30 s, and whether any shot ever measured a head (head_height not None) and
from which box.

## 2. Band (OWLv2): the stray alias and the shadow

The round 1 band config (section 4: "speaker upper body", "guitarist close-up",
"drummer wide"), 60 s headless, JSON to `scratch/r3/band.json`. Report:
- `max_subjects`, `subjects_created`, the `subjects` list (id, query, aliases, state, bbox),
  and every shot name. Expected: no "Guitarist N" shot whose subject is not a guitarist
  (round 2: "Guitarist 7 Close" on an audience member from 71 s). A non-guitarist subject may
  still list `guitarist` in its aliases; say if one does.
- For every band member's shots: `crop_height_min/max`, `limited_by_zoom` and
  `same_picture_as`, against round 2's band run. Band members whose boxes reach the bottom
  edge now get tighter, more often limited shots; name them.

Then rerun 60 s with temporary logging, stderr to `scratch/r3/band-trace.stderr`:
- in `SourceVision::on_detections` (crates/ac-engine/src/vision.rs): each batch's PTS and
  every detection (query, bbox, score), and every `NeedsTrackerStart` the store emits with
  the subject id;
- in the tracker's `step()` (crates/ac-vision/src/tracker/mod.rs): per subject per frame,
  score, gain, `surprising`, whether the match was trusted, contrast-held, ambiguous, the
  reported confidence and `held_at`.
Report:
1. the shadow subject (the one born on the wall shadow; round 2's subject 8): its box over
   time against the right guitarist's box (IoU per pass), whether it ever reaches the
   guitarist's head, the number of contrast-held frames, the number of restarts, and its
   largest gain at a trusted match;
2. for each real performer: the largest and the 99th-percentile gain at trusted matches
   (expected under 2), and the number of contrast-held frames (expected near 0), and any
   frame where a performer coasted at 0.3 for more than 15 frames in a row (when, and what
   the light did);
3. the pass nearest 71 s: every "a person" box with its score and every guitar box, so we
   can tell which matching mechanism gave the audience member the guitar box (the
   guitarist's own box missing; the guitar box reaching below the guitarist's feet; a tie
   on score; or the guitarist's box dropped as a group).

## 3. Light cues (optional, if the band clip has them)

If the band clip has a hard light change (a blackout, a spot snapping on, a strobe), report
from the step 2 trace the subjects whose confidence fell below 0.5 for more than half a second
around it, and whether any of them went Lost or split into a second subject.

## 4. SAM on the sermon

`config/sam3-local.toml` from round 1, 60 s headless, JSON to `scratch/r3/sam.json`. Report
`detector_ready_s`, discovery counts, the `subjects` list and every shot name. Expected: the
speaker's shots are still named "Guitarist N" when SAM boxes him as a guitarist in the pass
that admits him (that naming rule is waiting on Noah); say whether that happened. Check
`pgrep -f server.py` is empty afterwards.

## 5. The window (needs the screen unlocked)

If the screen is unlocked: open the window on the sermon config (no `--headless`), wait 60 s,
screenshot to `scratch/r3/window-sermon.png`, and report whether the speaker's Close tile shows
the LIMITED badge and whether Medium close and Medium look different from the Close. If the
screen is locked, skip this and say so.

## Report

Write everything to `scratch/mac-checks-round3.md` with the files under `scratch/r3/`, revert
the temporary logging, and send the cloud thread session a short summary by cross-session
message.
