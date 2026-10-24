# Mac checks, round 2, 2026-10-24

From the cloud thread session (the one you report to by cross-session message). Work in
`~/repos/auto-camera`. The old prototype folder (`/Users/nover/Documents/ChatGPT/Auto camera`)
stays read-only: read its footage and weights in place, never write there. Report numbers;
do not push to any branch. Temporary logging goes in uncommitted edits that you revert at the
end (`git status` clean). Leave no app, ffmpeg or Python process running when done.

Round 1 is in `scratch/mac-checks-2026-10-24.md`. What changed since, and what each step
checks:
- the subject store now merges two prompts' boxes of one person even when the pass lands
  about 200 ms late (the duplicate guitarists): step 1;
- a detector that is still loading shows as "preparing", sends nothing and counts no
  timeouts; a headless run waits for it before timing `--seconds` (up to 300 s, or the
  sidecar's `startup_timeout_s` when that is longer): steps 1, 2 and 5;
- the sidecar now gets 1195x672 from a 1280x720 or 4K frame (was 1008x567, which the model
  stretched to 672 lines): step 3;
- "speaker upper body" on OWLv2 carries a note that OWLv2 cannot tell who is speaking; the
  note is gone on the sidecar: steps 3 and 5;
- the headless summary has a `subjects` list, per-shot crop ranges and `same_picture_as`,
  and per-source `discovery_preparing` / `detector_ready_s`.

Reminder: `queries[].matches` in the summary is the newest pass only, one frame.

## 0. Update and build

`git fetch origin`. For each local branch (`video-pipeline`, `prompt-matching`,
`shot-words`, `sam-sidecar`) check `git log origin/<branch>..<branch>` is empty (stop and
report if not), then `git checkout <branch> && git merge --ff-only origin/<branch>`.
Expected heads, or later: video-pipeline d6c4cfc, prompt-matching 88a402c, shot-words
a31f43b, sam-sidecar 2dc8204. Run everything below on `sam-sidecar`: it contains every other
branch. Before building, note `ls -laT ~/Library/Caches/auto-camera/` and
`ls -laT <owl export dir>/coreml-cache/*/` (the OWLv2 export your round 1 configs use). Then
`cargo build --release -p auto-camera --features onnx`. The first OWLv2 run after this build
is the cold one that step 1 times, so do not run the app before step 1.

## 1. Band duplicates, and the first run after a rebuild (OWLv2, CoreML)

The band config from round 1, section 4 ("speaker upper body", "guitarist close-up",
"drummer wide"), headless 60 s, with `ORT_LOG=info` and stderr saved to
`scratch/r2/band-cold.stderr`:
`ORT_LOG=info ./target/release/auto-camera --config <band config> --headless --seconds 60 > scratch/r2/band-cold.json 2> scratch/r2/band-cold.stderr`

Report:
- `wall_seconds`, `detector_ready_s`, `discovery_preparing`, and `discovery_requests /
  applied / stale / errors / timeouts`. Expected: stale 0 and timeouts 0 (round 1 counted
  timeouts during the compile), and the run's 60 s start after the detector is ready.
- Whether stderr has "Model is already cached in .../coreml-cache/..." for both graphs
  (ORT's own cache hit), and whether a new `com.apple.e5rt.e5bundlecache` entry appeared
  under `~/Library/Caches/auto-camera/` (CoreML's per-build specialization, which is what
  a rebuild pays again).
- `max_subjects`, `subjects_created`, and the `subjects` list (id, query, aliases, state,
  bbox). Expected: each guitarist is one subject whose aliases include `speaker` when the
  "a person speaking" box landed on them, at most 6 subjects unless a false box takes a
  slot, and no subject that answers only `speaker` sitting on a guitarist's box.
- The shots per subject number: still 3 Guitarist Close and 1 Drummer Full; for any pair
  with `same_picture_as`, name both.

Then the same command again with no rebuild (`band-warm.*`): report `detector_ready_s`
(round 1's warm load was about 26 to 29 s to the first detection) and the same counts.

If a guitarist still shows as two subjects, rerun 60 s with a temporary `eprintln!` in
`SourceVision::on_detections` (crates/ac-engine/src/vision.rs) printing each batch's PTS and
every detection (query, bbox), and for each Created event the new box with every live
record's id, queries, bbox and IoU with it. Report the batch that created the twin.

## 2. The window while the detector prepares (OWLv2)

`touch apps/auto-camera/src/main.rs`, rebuild, and open the window on the band config (no
`--headless`). If the screen is unlocked, screenshot it every 20 s until the first detection
to `scratch/r2/window-*.png`. Report:
- whether each query row says "preparing the detector" (dimmed, not amber) and the status bar
  says "preparing detector m:ss" counting up, and whether either ever shows "detector not
  answering" or a timeout count during the load;
- whether the "speaker upper body" row shows the note starting "\"speaker\" asks the
  detector for \"a person speaking\", an action", under the parse line and in its hover.
Let the load finish before quitting, so CoreML keeps the compile.

## 3. SAM: send size and the speaker (branch sam-sidecar)

Direct client first (reuse round 1's `scratch/sidecar/` client). Take 20 sermon frames at
3 s steps across the same 60 s as round 1. For each frame send three versions, all with
"a person speaking", "a person playing guitar" and "a person" at threshold 0.3:
- (a) 1280x720 as decoded: `ffmpeg -ss T -i <sermon clip> -frames:v 1 -vf scale=1280:720 -pix_fmt rgb24`;
- (b) round 1's size: (a) then `scale=1008:567:flags=area`;
- (c) the new size: (a) then `scale=1195:672:flags=area`.
Report, per version: frames where a "a person speaking" box is on the labelled speaker (use
`scratch/pm/gt.json` where it covers the frame, otherwise judge by eye and say so), false
"a person speaking" boxes, and mean latency. Round 1's bake-off had 14 of 20 at 0.3.

Then the studio: `config/sam3-local.toml` from round 1 (sermon, queries "speaker upper body",
"guitarist close-up", "a person"), 60 s headless. Report `detector_ready_s`, discovery
counts, the `subjects` list (does any subject carry `speaker` as query or alias, and is it
the speaker?), the shots and their names, and each query's `notes` (expected: no OWLv2 note
on "speaker upper body"). Check `pgrep -f server.py` is empty afterwards.

## 4. Sermon framing: the unlimited Close that kept easing (OWLv2)

Round 1 saw the sermon's unlimited Close still moving at 60 s. A new shot starts on its own
target, so a Close that keeps moving means its target keeps moving. Add a temporary per-tick
`eprintln!` for that Close shot and its subject, CSV to `scratch/r2/close.csv`: time, shot
creation time, subject bbox (x, y, w, h), whether the subject looks truncated, tracker
confidence, Tracking or Lost, the target crop width from compose, the current crop width,
and for each applied pass the matched detection's height and whether the store reseeded the
tracker. Run 60 s headless on the round 1 sermon OWLv2 config. Report:
1. when the shot was created and the first applied pass;
2. the crop-width range over the last 30 s, and whether it drifts one way or jitters;
3. the largest |ln(track h / last detection h)| between passes, and the number of reseeds;
4. the number of Lost-to-Tracking changes;
5. from the summary: each shot's crop width and height range and `same_picture_as`.

## 5. Optional, if time allows

- OWLv2 scores for "a person speaking": on the 40 labelled frames, for every kept "a person"
  box dump its "a person" score and the best attached "a person speaking" score. For the 20
  sermon frames: in how many is the labelled speaker the top "a person speaking" score, the
  same ranked by speaking minus person, and the median gap to the runner-up; for the 20 band
  frames, the highest "a person speaking" score per frame against the sermon speaker's
  median. No code change follows from this round; it is input for later.
- Which ONNX Runtime the binary loads (`ORT_LOG=info` prints it; or `otool -L` on the
  binary) and whether two OWLv2 sources cold-loading at once take twice as long.
- SAM start cost: from the sidecar's stderr, "weights sha256 ... (A s)" and "model loaded at
  672 px (B s)" for one warm and one cold start.

## Report

Write everything to `scratch/mac-checks-2026-10-24-round2.md` with the JSON and stderr files
under `scratch/r2/`, revert the temporary logging, and send the cloud thread session a short
summary by cross-session message.
