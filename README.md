# Auto Camera

AI camera crew for live streams: text prompts pick subjects on each video
source, every subject gets smoothly framed virtual shots, and an operator
(optionally helped by an auto-director) switches them in a preview/program
(A/B) setup. Program goes out as an RTMP stream and a local MP4 recording from
one encode.

Status: rebuild in progress. The full pipeline runs end to end on synthetic
and file sources: discovery, tracking, framing, switching, the director,
compositing and output, driven by a native operator window or headless.
Design decisions live in [`docs/adr/`](docs/adr/); the runtime architecture
(threads, mailboxes, clocks, ownership) is
[ADR 0002](docs/adr/0002-pipeline-architecture.md).

## Quick start

On a Mac set up with `scripts/install-mac.sh`, from the repository root:

```sh
# 1. Make your studio once: a video file, or your cameras.
cp config/file-source.example.toml config/show.toml   # then set `path` to your video
# cp config/capture.example.toml config/show.toml     # cameras: see "Cameras and capture devices"

# 2. Check it, then run it.
cargo run --release -p auto-camera --features onnx,coreaudio -- --check
cargo run --release -p auto-camera --features onnx,coreaudio
```

With no `--config`, the app opens `config/show.toml` (not in git: each
machine has its own). If it is missing, the app says how to make one and
stops; it never falls back to the demo. The first run after a build spends
2 to 3 minutes on the detector's CoreML compile ("preparing detector").

In the window: type prompts such as `a person speaking` or `speaker upper
body` in the Queries panel; click a tile or press `1` to `9` for preview;
`Space` cuts and `Enter` (AUTO) transitions it to program, or drag the
T-bar; Output starts the recording and, with an RTMP URL, the stream.

## Demo

Requires Rust 1.92 or newer, and `ffmpeg` for file sources and output:
`AC_FFMPEG` / `AC_FFPROBE` name the executables, else they are looked up on
`PATH`, and on macOS also in `/opt/homebrew/bin` and `/usr/local/bin` (an
app started from Finder or the Dock gets a `PATH` without Homebrew). The
demo studio (`--demo`, [`config/demo.toml`](config/demo.toml)) is one synthetic 720p
camera with three moving subjects and a ground-truth detector, so it needs
no camera, model or video file. Its director starts in Suggest mode: it
proposes shots and the operator takes them.

```sh
cargo run -p auto-camera -- --demo           # operator window on the demo studio
```

The window is always dark and set in Inter, laid out after broadcast
switchers (vMix, ATEM Software Control, OBS) and sized for an operator
sitting back from the screen. A header bar carries Save show and the tally
lamps: the director's mode, ON AIR
(red while the stream is connected, amber while it connects, reconnects,
stalls or is down) and REC, each with the time the output has run, and
the studio clock; if the engine stops, a red banner says nothing is going
out. Below it are the preview (green) and program (red) monitors, each
framed in its tally colour with the shot name laid over the picture, with
the transition (Cut, Mix,
Dip and its rate), CUT and AUTO (lit red while a transition runs) and a
T-bar between them; under them the
multiview of every shot with tally borders, tracking status and quality.
On the right: the director (mode Off/Suggest/Auto, profile Talk/Music, the
current suggestion and why, Accept and Skip, and Release while a shot you
staged holds preview), the queries (add a prompt with its
framings, remove one; pin, hide or show subjects) and the output (RTMP URL,
recording path, start/stop, encoder statistics and, while it streams, the
stream's connection: "stream connected · 3980 kbit/s", or in amber "stream
reconnecting (attempt 2) · 1 outage" after the ingest went away). Under the prompt a live
line says how it reads, for example "Detector sees: a person speaking ·
Framing: Medium, Medium close (person)", and the framing checkboxes follow
the prompt's shot words until you tick one yourself (see
[Prompts](#prompts)). Under each live query, a line says what the newest
detector pass found for it ("3 matches · best 0.71", or "no matches" in
amber, per source when there are several), so a prompt the detector does
not understand shows up at once. A tile whose preset's zoom ceiling keeps it looser
than its size carries an amber LIMITED badge in the corner of its picture,
and the monitors and the director's suggestion name it "Speaker 1 Close
(limited)"; its hover text says when it looks the same as another of the
subject's shots (two sizes both drawn at the ceiling). The status bar shows
source frame rates, tick timing and dropped frames.

Above it, the status bar lists alarms (spec M14), one row each, for as long
as their cause lasts: the program black for 2 s while the output runs, the
program on a subject lost for half a second, the program's source sending
no new frame for over half a second, encoder drops, a failed output, the
stream reconnecting, stalled (the ingest takes no packets) or down, a
recording without audio (no audio track, or no source audio for 2 s), and
program audio unheard (it connected, or came back, at another rate than
the output was started with: restart the output). Most clear only once
their cause has been gone for a while (a second for the picture and
audio, five for drops and for a stream taking packets again),
so they do not flicker; a failed output clears when the output starts
again, and a stream that is down when the session ends. Acknowledge dims an alarm and removes its button,
but it stays listed until its cause clears; one that comes back is new.
With `start_on_wide = true` in the `[switcher]` table the first source's
wide is on program from the start (it shows black until the source
delivers), so a studio restarted after a crash goes out on a clean wide
instead of black. Scoring a trace of such a run (`ac-eval`) counts that
black as the start-up wait, not as a loss on program.

| Input | Action |
|---|---|
| click a tile | stage it on preview |
| double-click a tile | cut it straight to program ("Cut now") |
| right-click a subject tile | pin, hide or re-acquire the subject |
| `1`–`9` | stage tile 1–9 on preview (during a transition, for its end) |
| `Space` | cut (during a transition, cut it short); refused if preview changed since you saw it |
| `Enter` | auto transition (during one, queue a take of the shot staged meanwhile); refused likewise |
| `Esc` | leave a focused field or control, so the shortcuts work again |

Shortcuts are paused while a field or control has the keyboard (a prompt or
number being edited, or a control reached with `Tab`), and the status bar
then says "shortcuts paused"; they are also ignored on key repeat and with
Ctrl, Alt or Cmd held. While the output is starting or running, closing the
window or clicking Stop asks first, so a stray click cannot end the show; a
termination signal (Ctrl-C, `SIGTERM`) still quits without asking.

A running transition does not block the controls. A shot staged during
one waits for its end and then lands on preview (the preview caption says
"next …"); Auto queues a take of it, which fires once, when the transition
ends, if the shot is still available, and is otherwise dropped with a
notice saying why. Cut cuts the transition short: the shot it brings in,
which the preview monitor shows, lands on program at once, provided it is
still available; a shot staged meanwhile then lands on preview, and a take
queued of it is cancelled. A double click on a tile still puts that tile
on air at once, ending the transition in the same step. The T-bar and the
clock never share a transition: the T-bar cannot take over a running Auto,
and while the T-bar holds a transition CUT is disabled, and `Space` and a
double click on a tile are refused with a notice (the lever finishes or
abandons it; the double click's first click still stages the tile for the
end).

Preview is the operator's. Cut and Auto (the buttons, `Space` and `Enter`)
take the shot the window showed on preview when you pressed (or the shot
you had just staged, while the picture has not caught up), and Accept the
suggestion the panel showed; if that changed in between (the director
restaged preview or moved its suggestion on a moment earlier), the take is
refused with a notice, so none of them puts on air a shot you did not see.
The T-bar is not fenced: it takes whatever preview holds when you start to
move it. A shot you stage holds preview against the director: in Suggest
mode it never stages a suggestion over yours until you take it or click
Release, and the status bar says what it held back. Skip leaves the current
suggestion out of the suggestions for `skip_s` seconds. While the director
runs in Auto mode the program monitor carries a red AUTO badge, and your
own switching (a key, a click on a tile, Cut, Auto or the T-bar) takes over
as `takeover` says: by default it drops the director to Suggest; `"pause"`
holds its takes for `pause_s` seconds after each action, counted down on
the badge; with `"director"` it carries on and stages its shot over yours.
A notice says which happened.

Settle-before-take is off by default. With `[switcher.settle]`
`enabled = true`, a Cut, Auto or Accept pressed while the preview crop is
still travelling to its subject (its settledness below `threshold`, 0.6 by
default) waits for it to settle, at most `max_wait_s` (0.5 s by default and
at most), and the preview caption says `waiting for preview to settle`.
Press again (`Space`, `Enter`, CUT, AUTO or Accept) to take it at once; a
press in the half second after the take went on air by itself counts as
that second press and does nothing (but `Space` lands a mix still
running), so it never cuts straight back.
Staging another shot puts the waiting take on air first, as it would have
gone at once without the rule. A double click on another tile is a staging
and then a Cut now, so the waiting shot is on air for the length of the
double click before the tile cuts in. A T-bar move never waits and replaces
a waiting take, unless it does nothing (the lever at its rest end). If
preview changes while a take waits, it is dropped with the same notice as
any fenced take. The director's own takes never wait. Whether or not it is
on, the preview caption turns amber and says `subject N lost` when the
preview shot's subject is lost; it only warns, and the take goes ahead.

## Headless mode

```sh
cargo run -p auto-camera -- --demo --headless --seconds 5 --record /tmp/demo.mp4
auto-camera [--config PATH | --demo] --headless [--seconds N] [--record PATH] [--trace PATH]
```

A headless run starts the engine without a window, waits for the first
picture, puts a shot on air (the director does it in Auto mode, otherwise
the first wide is cut to program), waits up to five minutes for the
detector to load (a CoreML compile can take minutes; the run fails if it is
still loading then), starts recording when `--record` is given
(the stream also goes to `output.rtmp_url` if the config sets one), runs for
`--seconds` (default 10) after recording started, finalizes the recording,
shuts down and prints a JSON summary on stdout. Besides counters, each
source gives its `state` and the `error` that keeps it off the air (`null`
while it delivers), and says whether its detector was still preparing and
when it was ready (`discovery_preparing`, `detector_ready_s`), `queries`
says how each prompt was read (with the parse's `notes`), each shot gives
the range of its crop size over the run (`crop_width_min` to
`crop_height_max`) and whether the zoom ceiling limits it
(`limited_by_zoom`, with `same_picture_as` naming the shot it then looks
the same as), `subjects` lists the subjects at the end with the queries
they answer, and for a stream `output.stream` says how its connection
stood while the output last ran (`state`, `attempt`, `stalled`,
`bitrate_kbps`, which is `null` on an ffmpeg older than 6.0) and how often
it was lost and opened again (`outages`, `reconnects`):

```json
{
  "ok": true,
  "ticks": 160,
  "sources": [{ "id": "stage", "state": "running", "error": null, "fps": 31.0, "frames": 155, "missed": 3, "discovery_preparing": false, "detector_ready_s": 0.03, "discovery_applied": 40 }],
  "queries": [{ "prompt": "person", "detector_phrase": "person", "presets": ["medium", "close"], "basis": "person",
                "parse": "Detector sees: person · Framing: no shot words (person)",
                "matches": [{ "source": "stage", "count": 3, "best": 0.9, "seq": 148, "pts_seconds": 4.933 }] }],
  "max_subjects": 3,
  "program": { "last": "stage/wide", "changes": 1, "log": [[0.23, "stage/wide"]] },
  "director": { "mode": "suggest", "profile": "talk", "takes": 0, "reason": "best available shot" },
  "timing": { "budget_ms": 33.33, "avg_ms": 6.52, "max_ms": 19.44, "late_ticks": 0, "skipped_ticks": 0 },
  "output": { "state": "finished", "frames_written": 151, "frames_dropped": 0, "encoder": "libx264", "exit_code": 0 }
}
```

(abridged). Each query's `matches` is the prompt feedback of the window:
what the newest applied detector pass of each source found for it, and on
which frame. The exit status is 0 on success, 1 when the engine, the output
or the trace failed or a source never delivered a picture during the run
(`ok` is false and `error` says why), 2 for usage and configuration errors.
Recordings are never overwritten: an existing `--record` path is an error.

### Per-tick trace

`--trace PATH` (headless or in the window) writes what the engine did on
every tick as JSON Lines, one compact JSON object per tick, for scoring
framing offline with `ac-eval` (spec V9: containment, framing error, jerk,
latency):

```sh
cargo run -p auto-camera -- --demo --headless --seconds 2 --trace /tmp/demo.jsonl
```

Each line has the tick number and its PTS (`tick`, `pts_ns`), whether it
started late and how many ticks were skipped before it (`late`,
`skipped`), each source's newest frame and its subjects with box and track
state, each shot's `target` (the ideal crop for its subject's box, `null`
while the subject is lost) and its `crop` (where the smoothing actually
drew it), and the program, preview and running transition. The schema is
`ac_eval::TraceRecord`; its docs show a full record. The line count equals
the summary's `ticks`, which the summary's `trace` repeats (`records`, with
`missing` for ticks a writer that fell over half a minute behind had to
drop). The file must not exist, as for `--record`; a run whose engine does
not start (a setup no retry mends, such as a detector sidecar whose program
is missing) removes its empty trace, so the same command works once the
cause is fixed. A source that cannot be opened does not stop the engine (it
starts offline and is retried, see "Studio configuration"), so a run with a
missing file ticks and, like every run that ticked, keeps its trace: move
or remove it before running the same command again.

### Recorded detections and the eval runner

`--record-detections PATH` (headless or in the window) writes every
detector pass the engine applied as JSON Lines: the source, the frame's
sequence number and PTS, the queries looked for, and each detection's
query, box, score and extent (`ac_vision::PassRecord`). The summary's
`detections` gives the lines written (`records`, with `missing` for passes
a writer that fell behind had to drop). As for the trace, the file must not
exist, and a run whose engine does not start removes it.

The eval runner (`apps/eval-runner`) steps the engine over a studio's
sources on simulated time, tick after tick on one thread, so the same input
always gives the same trace, and it can answer discovery from such a log
instead of from a model, so scoring needs no inference:

```sh
# Record once, with the studio's own detector ...
cargo run -p eval-runner -- --config config/demo.toml --seconds 10 \
  --trace /tmp/live.jsonl --record-detections /tmp/passes.jsonl
# ... and replay without it: the same trace, byte for byte.
cargo run -p eval-runner -- --config config/demo.toml --seconds 10 \
  --trace /tmp/replay.jsonl --replay /tmp/passes.jsonl
```

Synthetic sources are drawn, and file sources decoded frame by frame, in
step with the ticks: each tick shows the newest frame due by then. A
discovery request is answered `--latency-ticks` ticks later (default 1),
and the first wide goes on program as in a headless run. A replayed pass
answers a frame at most half a frame interval from it, so a log the runner
recorded matches frame for frame; a log the app recorded live looked at
whichever frames real time handed its detector, so replay it with a wider
`--tolerance-ms` (half the discovery interval, say). A request with no pass
is a miss, which changes nothing, so a pass left out of a log skips just
that update. The run ends after `--seconds`, or once every file source has
played to its end, and prints a JSON report: ticks, and for each source the
requests, applied passes, misses and detector errors. The sidecar detector
answers in real time and cannot run in the runner: record it with the app
and replay it.

A file source can play part of its file as a clip of its own: `start_s`
seconds in (default 0), for `duration_s` seconds (default: to the end).
The first frame at or after `start_s` comes first, with PTS 0, and the
audio starts with it, so a segment of a long recording is scored like a
short clip, against the labels of the whole file once the scoring scope
gives the same `start_s` (`ac_eval::Scope`), and a detection log recorded
on the segment replays on it. The cut is exact in MP4 and Matroska
files, and in a container without a seek index, such as an MPEG-TS
recording, when it has a keyframe at least every 15 s. A `start_s` below
0 or a `duration_s` not above 0 is a config error, as a bad `max_width`
is: no retry of the file would mend it, so the load and `--check` refuse
it rather than leave the source offline.

## Staying awake while live

On macOS the app keeps the Mac from sleeping while program goes out: it
runs `caffeinate -i -m -s -w <its pid>` (no idle, disk or system sleep)
while an output session runs in the window, with `-d` added because the
window's monitors show program, and for the whole of a headless run, from
before the detector loads until the recording is final. `caffeinate` is a
supervised child like ffmpeg: it is killed and reaped when the session or
run ends, and `-w` ends it if the app dies. Other platforms have no
`caffeinate`; the app says so on stderr and in its log and runs on, as it
does when `caffeinate` cannot start or ends early (the window when an
output session turns out not to be held, a headless run at its start or
its end). The engine's output view (`keep_awake`) says whether a live
session is held, by the engine's own `caffeinate` or by the headless
run's, and if not, why. This does not stop App Nap from throttling a
minimized window: that takes `LSAppNapIsDisabled` in an app bundle's
`Info.plist`, and the repository builds no bundle yet.

## Readiness check

```sh
cargo run -p auto-camera -- --check --config config/sam3-sidecar.example.toml
auto-camera [--config PATH | --demo] --check
```

`--check` finds what would stop a studio before a show, without starting
it. It lists the problems in the config's values, all of them rather than
the first (the `lexicon`, `tracker`, `framing`, `subjects` and `director`
tables, which the crates that use them check, give their first problem
each), and looks at everything the config names: each file source's video
and each audio input's file (and that a device audio input is in a build
that can open it), the ffmpeg tools (`-version` of the output's
ffmpeg, of the ffmpeg and ffprobe file sources and audio inputs decode
with, and of that ffmpeg when capture sources list and capture their
cameras with it), the detector's model files (and
that an OWL detector is in the build), the sidecar's program, its venv (a
python link that points nowhere, a base Python that is gone) and the paths
in its command, and that `output.record_path` does not exist yet, in a
folder that takes new files. Whether a camera is connected is not checked,
since it can be plugged in later. It opens no camera, source or model,
starts no sidecar and contacts no RTMP server: it runs nothing but the
ffmpeg tools' `-version`, and writes nothing but an empty probe file in the
recording folder, removed at once. Each finding starts a line of its own
(a longer message, such as a parse error's excerpt of the file, goes on
indented under it):

```text
Checking config/sam3-sidecar.example.toml
  ok       config: schema 1, 1 source, 3 queries, detector "sidecar"
  problem  source "cam-1": config/../media/rehearsal.mp4 does not exist
  ok       output.ffmpeg and file sources' ffmpeg (AC_FFMPEG): ffmpeg runs (ffmpeg version 7.1.1)
  ok       file sources' ffprobe (AC_FFPROBE): ffprobe runs (ffprobe version 7.1.1)
  problem  detector.command[0]: config/../models/sam3/.venv/bin/python does not exist; make the sidecar's venv as its README says
  ok       detector.command[1]: config/../sidecars/sam3-mlx/server.py is readable
  problem  detector.command[5]: config/../models/sam31-mlx does not exist
  problem  output.record_path: folder recordings (from the working folder) does not exist (starting the output does not create it)
4 problems found; fix them and check again.
```

The exit status is 0 when nothing was found that would stop the studio
(warnings allowed) and 2 otherwise. Whether a model file holds the right
weights (a hash check) is not part of it yet.

## Logs and crash reports

Every run, in the window or headless (not `--check`, which starts
nothing), appends to a log in a per-user folder:
`~/Library/Logs/auto-camera` on macOS, `$XDG_STATE_HOME/auto-camera`
(else `~/.local/state/auto-camera`) on Linux; `--log-dir PATH` picks
another. The current file is `auto-camera.log`; at 8 MiB it moves to
`auto-camera.1.log` and the four newest old files are kept. A second app
running at the same time (the window beside a headless rehearsal) writes
`auto-camera-2.log` instead, and so on up to four. One line per event, in
UTC: the app's version and commit, the config's SHA-256 (for a show,
also that of the studio file it lies over) and the model files (path,
size and time at once, their SHA-256 from a background thread, so a
start never waits for gigabytes of weights), which lines start every
file again after a rotation, then what the studio did (sources and audio
inputs starting, going offline and stopping, with the reason, detectors
ready or failing, program changes, the output's lifecycle and the
stream's connection), every line ffmpeg and the detector sidecar print on
stderr (stream key redacted; a source decoder's repeats of one complaint
are written at most once a minute, with a count), the sidecar's starts,
losses and restarts, and the warnings and errors the app prints on stderr
(the config's warnings, a system not kept awake while live, a termination
signal, an engine that failed or did not shut down cleanly, a show saved
or not as the window closed). A panic writes `crash-<time>-<pid>-<n>.txt`
next to the log, with the panic, its backtrace and a summary of the
studio's newest state, and prints its path; each start keeps the 20 newest
reports. Nothing is sent anywhere. A headless summary also reports the
detector sidecar, when the studio has one (`sidecar`: `state`, `pid`,
`model`, `spawns`, `restarts`, `failures` and the other host counters).

## Studio configuration

A studio is one TOML file (or JSON, by extension). It starts with
`schema = 1`, the layout its keys follow. A file without the key (any file
written before it existed) still loads, as schema 1, with a warning on
stderr and in the window's status bar to add it; a newer or unknown schema
is an error that says which one this build reads, so an older build never
half-reads a newer file. Every section but `sources` is optional, unknown
keys are errors, and relative paths are resolved against the config file's
directory, except `output.ffmpeg` and `output.record_path`, which, like a
path typed at Start Output, are taken from the working folder (a bare
`ffmpeg` from `PATH`). A studio fed by a local video
file ([`config/file-source.example.toml`](config/file-source.example.toml)):

```toml
schema = 1

[[sources]]
id = "cam-1"
name = "Rehearsal"
kind = "file"                # or "synthetic" (width, height, fps, subjects), or
                             # "capture" (a camera; see "Cameras and capture devices")
path = "../media/rehearsal.mp4"
loop_playback = true
max_width = 1280
audio = true

[[queries]]
name = "Speaker"
prompt = "a person speaking at a lectern"
presets = ["medium", "close"]  # wide is always there; also full, medium_close,
                               # or { id, name, size, headroom, max_zoom }
priority = 4                   # 1 to 5, higher first: ahead of the drummer (3)

[[queries]]
name = "Drummer"
prompt = "drummer wide"        # no presets: the shot word "wide" picks Full, and
                               # the detector is asked for "a person playing drums"

[detector]
kind = "owl"                 # "sidecar" (SAM 3.1, below), "synthetic" (ground truth) or "none"
model = "../models/owlv2-base-onnx/model.onnx"
tokenizer = "../models/owlv2-base-onnx/tokenizer.json"

[discovery]
rate_hz = 1.0                # detector passes per second per source

[director]
mode = "suggest"             # off, suggest or auto
profile = "music"            # talk or music
takeover = "suggest"         # what your switching does in Auto mode: suggest, pause or director
pause_s = 10                 # with takeover = "pause", seconds each action holds its takes
skip_s = 30                  # seconds a skipped suggestion stays out

[output]
width = 1280
height = 720
fps = 30
audio_source = "cam-1"       # program audio from this source
# rtmp_url = "rtmp://live.example.com/app/STREAM-KEY"
record_path = "recordings/show.mp4"
```

A query's `priority` ranks its subjects for the director: a whole number
from 1 to 5, higher first. When no query gives one, the list order is the
ranking: the first query has 5 and each later one a level less, down to 1.
Once any query gives one, a query without one has 3. Each level a
subject's query stands above the lowest-ranked query that has subjects
adds 0.05 to the score of its shots, so at equal quality the
higher-priority prompt's shot is suggested (and taken, in Auto mode),
while a shot tracked clearly better can still win: four levels are worth
0.2 of quality. A subject is ranked by the query that first found it, not
by other prompts that also box it (one stray box would otherwise rank the
wrong person up). Only differences count: queries that all share one
priority are directed exactly as before priorities existed, a query that
finds no one changes nothing, and a wide counts as the lowest. A query
added in the window joins the end of the list (or gets 3 when priorities
are given), and a priority outside 1 to 5 is a configuration error.

Further sections tune the `tracker`, `subjects` (limits and lost/retire
times), `framing`, `switcher` (default transition, `start_on_wide`,
settle-before-take), `monitors` (monitor and multiview tile sizes and
rates) and `lexicon` (below); see `crates/ac-engine/src/config.rs`. Every
value is checked when the file loads; the `output` table with the
encoder's own ranges (even sizes up to 7680x4320, 1–120 fps,
`video_bitrate_kbps` 100–100000, `audio_bitrate_kbps` 32–512,
`keyframe_seconds` 1–10, an `rtmp://` or `rtmps://` URL), so a bad value
fails at load rather than when the output starts.

A source that cannot be opened when the engine starts (a file that is not
there yet) does not stop it: the other sources run, the window's status
line shows that source `OFFLINE` (or `RECONNECTING`) with the reason, and
it is retried with a backoff of 0.5 s doubling to 8 s, so it goes on air
once its input appears. A source that loses its input while running (its
ffmpeg killed or crashed) is reopened the same way. If it is the
`audio_source`, an output started after it came on air carries its audio.
Only a source setting no retry can fix (a synthetic pattern that cannot be
made, a `max_width` below 2, a capture request no camera could deliver)
stops the start.

Program audio is one stereo pair. `output.audio_source` names a source,
for its embedded audio (not a capture source: a camera's audio is not
captured), or an audio input: an audio-only feed such as the soundboard's.
That is the board itself on the Mac (an audio device, below), or a WAV or
RF64 recording of the board, played as a live feed:

```toml
[[audio_inputs]]
id = "board"                 # not a source's id
name = "Soundboard"
kind = "file"
path = "../media/board-rehearsal.wav"
loop_playback = true
channels = [3, 4]            # left and right, counted from 1; [5] puts 5 on both sides
channel_count = 16           # optional: what the input has, checked before use

[output]
audio_source = "board"
```

The picked channels are copied as they are; nothing is mixed. Without
`channels` the program takes channels 1 and 2, or a mono input's one on
both sides at full level. A pick beyond `channel_count` fails the load; an
input that opens without the picked channels, or with another count than
`channel_count`, stays offline with the reason and is retried, like a
source. The log says when an input goes offline, comes back or stops,
with the reason (`audio input board: running -> offline (...)`), and
writes a reason that every retry repeats only once. Program audio reaches
the encoder at the input's own rate (48 kHz while the input has not
connected), and the encoder converts it to 48 kHz AAC. The engine never
waits on an input: a stall is filled with silence and the late audio after
it cut, so the program stays in sync.

An input on its own sample clock (a board's USB interface, typically tens
of ppm off the engine's clock; 100 ppm is a second over three hours) is
corrected for drift: the engine measures the drift from the blocks'
timestamps against their sample counts and resamples the input by that
much, before the pacing above, with a band-limited (windowed-sinc)
resampler whose ratio changes smoothly, up to 1000 ppm. In simulation a
board 100 ppm off stays within 20 ms of the video for three hours with no
silence inserted and nothing cut. A step in the timestamps is never taken
for drift: up to 100 ms (audio a full ring dropped, a short loss) it is
made up once the next few blocks confirm it, with that much silence (or,
for timestamps that step back, that much cut), from a connection's third
block on as later, and the drift measurement carries on; beyond 100 ms (a
reconnection, a long loss) it is a break: the pacing above fills the gap
and the measurement starts again. A loss close to 100 ms can run the
pacing out of slack before the blocks after it confirm the step; it then
fills the gap itself and cuts exactly the silence it filled from the late
audio (splitting a block where the cut ends), so the gap is made up once.
A timestamp that strays and comes back within three blocks (a glitch in
the device's clock) is not a step: no block is edited for it. One that
stays off for four blocks or more (spanning at least 6 ms, should its
blocks be shorter than 2 ms) is taken for a step and made up, then taken
back once four blocks confirm the way back: two short edits of its size
(silence and a cut), audio off by that much for about as long as the stray
lasted, and no lasting offset. Audio the pacing drops at its 100 ms limit
while that silence is in is given back as silence once it is out, if that
is within a second of the drop, while the pacing owes no silence of its
own, and with audio then no more than 50 ms ahead (where it was before the
step); otherwise it stays dropped, and the program plays that much early.
What is not corrected: blocks within a dead band of 2 ppm pass through bit
for bit, as a file input on the engine's clock always does; a source's
embedded audio is never resampled, and drift of the video itself (a camera
against the engine) is not corrected; drift beyond 1000 ppm is left to the
fills and cuts. The window's output panel shows the program input's
correction (below zero for a board whose clock runs fast), its resets and
the steps made up, with the trouble a running input delivered through
(a device's discontinuities) on a weak line under it. While the input is
not running, it shows its state and why instead, in amber; while it is not
heard for its rate, it shows only why. A headless summary reports them for
every input (`audio_inputs`: `id`, `name`, `program`, `correction_ppm`,
`drift_resets`, `drift_steps`).

On the Mac the board is an input of `kind = "device"`: its USB audio
interface is a CoreAudio device, and so is anything else with a CoreAudio
driver (a Dante Virtual Soundcard, an AVB or MADI interface). It is named
by its CoreAudio UID, which stays the same when it is replugged and which
`--list-audio-devices` prints for every input device, ready to paste:

```sh
cargo run --release -p auto-camera --features coreaudio -- --list-audio-devices
```

```text
2 audio input devices:
  MacBook Pro Microphone (the default input): 1 input channel at 48000 Hz
    uid = "BuiltInMicrophoneDevice"
  SQ: 32 input channels at 48000 Hz
    uid = "AppleUSBAudioEngine:Allen&Heath:SQ:1A2B3C:1"
Copy a device's uid line into its [[audio_inputs]] entry, with kind = "device".
```

```toml
[[audio_inputs]]
id = "board"
name = "Soundboard"
kind = "device"
uid = "AppleUSBAudioEngine:Allen&Heath:SQ:1A2B3C:1"
channels = [1, 2]            # the board's stereo bus on its USB channels
channel_count = 32           # optional, as for a file
```

The device is captured with all its input channels at the rate it runs at,
and each block carries the time it was captured, on the engine's clock:
the host time CoreAudio stamped it with, less the device's latency as the
connection's first buffer had it (a few to tens of milliseconds, the same
for every block of the connection, so only how the times advance
matters). The rate is kept, but the cpal library that opens the
stream switches a device that offers a 32-bit float format at that rate to
it: a setting of the device that stays after the show and that Audio MIDI
Setup shows. Only the device with exactly that UID is opened: one that is
not connected leaves the input offline, retried like a source, so the
board comes on air within seconds of being plugged in, and back after it
was unplugged; the default microphone is never opened in its place. On
air, it is heard if it runs at the rate the output was started with: the
board's own if it was connected then, 48 kHz if the output started before
it was. A board at another rate (one plugged in at 96 kHz after the output
started without it, or one whose rate was changed during the show) is not
heard until the output is restarted, which its error and the alarm
"program audio unheard" say. The log says why it is offline, with the
UIDs of the devices that are connected, and says it again when that list
changes. A glitch the input delivers through (audio dropped because the
input fell behind its device, a jump in the device's timestamps, an
overload CoreAudio reports) keeps it on air: it is counted, kept as the
input's error (`audio discontinuity (3 so far; the device reported 2 overloads): ...`)
and written to the log, at most a line every 10 s. Overloads are counted
and logged apart from what the input measures, since one that lost audio
also shows as the gap it left, and the line saying how much it lost must
not wait behind the overload's. A timestamp jump outside the recent
jitter range (at least a millisecond) is reported half a second after it,
once it has lasted; timestamps that are off for a few blocks and then
back, in one step or
several, are one timestamp glitch, with no audio missing and the input's
timeline as it was, and timestamps that only scatter by more than a
millisecond move nothing for good. A recent median timeline prevents
short noise bursts or late ramps from moving the map, while a lasting
rewind remains a rewind under the noise already present. The first
callback stays provisional even when it is split into smaller blocks.
A device that sends nothing for 2 s is reopened. Device inputs need
macOS and a build with
`--features coreaudio`, which is opt-in like `onnx`; any other build
loads the config, `--check` reports the input as a problem saying how to
build, and the input stays offline with that message. macOS asks once
whether the app (or the terminal it runs in) may use audio input;
refused, the input runs on silence, until it is allowed in System
Settings, Privacy & Security, Microphone.

A source's embedded audio is mixed to stereo by ffmpeg as it is decoded,
with the matrix the encoder used on it before: a stereo track as it is, a
mono one on both sides at −3 dB, a 5.1 one with its centre and surrounds
folded into left and right at −3 dB and its LFE left out. A config
without `[[audio_inputs]]` therefore sounds as before. One reading
changes: for a track of more than two channels the loudness meter now
reads the stereo mix the encoder is given, not the track's own channels.

### Cameras and capture devices

On macOS a source can be a USB/UVC webcam or an HDMI-to-USB capture
dongle, read through ffmpeg's avfoundation input
([`config/capture.example.toml`](config/capture.example.toml)):

```toml
[[sources]]
id = "cam-1"
kind = "capture"
unique_id = "0x14200000046d085e"  # system_profiler SPCameraDataType: "Unique ID"
width = 1280                      # the mode to ask the camera for
height = 720
fps = 30
# pixel_format = "nv12"           # optional; uyvy422 otherwise
```

A camera is named by its unique ID, never by ffmpeg's device number, which
changes whenever cameras are plugged or unplugged: before every open and
every reconnect the source lists the cameras (`system_profiler
SPCameraDataType -json` and `ffmpeg -f avfoundation -list_devices true -i
""`) and finds the number the ID has now. Two cameras of one model have the
same name in both lists and are told apart by their order, which the two
lists are assumed to share. A camera that is not connected, or cannot be
told apart, leaves the source offline with an error listing the connected
cameras and their IDs; another camera is never opened in its place. macOS
builds a USB camera's ID from the port it is plugged into, so plug it back
into the same port.

The size and rate are required and, with the unique ID and the pixel
format if one is given, checked when the file loads, so a request no camera
could deliver (an empty ID, a size over 8192, a pixel format ffmpeg's
avfoundation input does not know) stops the start. Without a pixel format
the source asks for uyvy422, which macOS offers for webcams and capture
dongles, rather than leaving it to ffmpeg, whose own default (yuv420p) they
refuse. A mode the camera does not offer fails the source at once, with
ffmpeg's answer as its error: the reason, then the modes the camera has (up
to 64 lines of them; the error is in the log too). A camera whose ffmpeg
exits, or that sends no picture for 3 s (10 s after opening), is reopened
like any failed source. Camera audio is not captured yet.

## Shows

```sh
auto-camera [--config PATH] [--save-show PATH]                # Save show in the window
auto-camera --config sunday.show.toml [--check]               # open or check a show
```

A show is what the operator built on a studio: the queries (with their
presets, literal flags and priorities, those added while it ran
included), the director's mode and profile and the transition. The
window's **Save show** button writes it to a show file; nothing is saved
on a change, and a headless run, which changes none of these, saves
nothing. A show file is TOML (or JSON, by extension) that starts with
`show = 1`, its own layout number, and names the studio file it lies
over:

```toml
show = 1
studio = "/Users/me/studios/sunday.toml"

[[queries]]
name = "Pastor"
prompt = "speaker upper body"
presets = ["medium", "medium_close"]

[director]
mode = "auto"
```

The studio file stays the base. A show never holds the RTMP URL, so the
stream key stays in the studio file alone, and a show that gives
`rtmp_url` is refused. A saved show holds `sources` and `[output]
record_path` only where they differ from the studio file's (a show
written by hand with its own), so a camera added to the studio file, or
a recording path changed there, is in the show the next time it opens.

`--config` and `--check` take a show file as they take a studio file: the
studio file is read, the lists the show gives (`sources`, `queries`)
replace its lists, the keys it gives in `[director]`, `[switcher]` and
`[output]` replace those values, and the result is checked like any
studio config. As with a studio file's `schema`, a show of an older
layout is upgraded as it is read, with a warning (1 is the first), and a
newer one is an error.

Save show writes to `--save-show PATH` when given, back to the show file
the window opened, and otherwise to the `shows` folder of the data dir,
named after the studio file and a short hash of its full path (as in
`sunday-4b3716bc.show.toml`), so studio files named alike in different
folders get shows of their own. The data dir is `AC_DATA_DIR` when set,
which must be an absolute path (a relative one makes Save show fail and
say so), else `~/Library/Application Support/auto-camera` on macOS and
`$XDG_DATA_HOME/auto-camera` or `~/.local/share/auto-camera` elsewhere.
A show that would not open is not written, and a file that is not a
show, a show this build cannot read (one a newer build wrote), the show
of another studio file and the studio file itself are never replaced.
The write goes to a temp file beside the target, readable by its owner
only (`0600`), synced, then renamed over the target, so a failed save
leaves the previous show as it was. The status bar says where the show
went, or why it did not.

## Prompts

A prompt says who or what to frame, and may also say what shots to make
([ADR 0003](docs/adr/0003-shot-words-in-prompts.md), proposed). The prompt
is read once, when the query is created, and the detector never sees the
operator's text, only what the parser makes of it:

| Prompt | Detector sees | Framing |
|---|---|---|
| `speaker upper body` | a person speaking | Medium, Medium close (person) |
| `speaker's upper body at the lectern` | speaker at the lectern | Medium, Medium close (person) |
| `guitarist close-up` | a person playing guitar | Close (person) |
| `guitar close-up` | guitar | Close (object) |
| `drummer wide` | a person playing drums | Full (person) |
| `a person playing guitar` | a person playing guitar | the query's presets (person) |

- **Shot words** count at either end of the prompt or right after a
  possessive, in any case, with or without hyphens, singular or plural
  ("close-ups"): close, close-up, CU, head and shoulders, headshot, tight,
  face (Close); medium close, MCU, chest up, bust (Medium close); medium,
  mid shot, MS, waist up (Medium); upper body, half body (Medium and Medium
  close, until Noah picks one); full, full body, full length, head to toe,
  long shot, FS (Full); and "wide" after a subject (Full for now; framing
  the subject with its kit is later work). A prompt that is only shot words
  is refused: write "the face", or set `literal = true`, to look for a face
  itself. Where a shot word may be an everyday word ("full band", "a rock
  face", "Ms Smith") the parse says so; `literal = true` sends such a prompt
  as typed.
- **Role words** are rewritten when they are the whole subject: speaker,
  pastor and preacher become "a person speaking", guitarist "a person
  playing guitar", drummer "a person playing drums", singer "a person
  singing". Each is a hypothesis until it is benchmarked on footage.
  Speaking and singing are actions, which OWLv2 cannot see: every person
  gets such a query's shots, and the parse carries a notice saying to take
  the speaker's shot by hand. The sidecar (SAM 3.1) sees actions, so on it
  the notice is not shown.
- **Basis.** A subject whose head noun is a person or role word is framed
  as a person: on a whole-person box, shots are cut at body lines measured
  in head heights from the crown (Close at the shoulders, Medium at the
  waist), with the head estimated from body proportions. Anything else
  ("guitar close-up") scales its box, as before, even when people-first
  matching answers it with the whole player: until the guitar's own box is
  attached, that "Close" is the player's full length and is not marked.
- **Presets.** A query's explicit `presets` always win over its shot words;
  with neither it gets medium and close. `literal = true` sends the prompt
  as typed. Prompts without shot or role words keep their detector phrase
  and presets; if they name a person and the detector returns whole people,
  their shots are now cut at body lines too, and may be marked limited.
- **One subject, several queries.** Prompts that ask the detector for the
  same thing ("speaker close-up" and "speaker wide" both ask for "a person
  speaking") find the same person, who becomes one subject answering both
  queries and gets the shots of both, each named after its query ("Speaker
  tight 1 Close"). A shot on air keeps how it is framed when its query is
  removed or replaced.
- **Lexicon.** The words are data. A `[lexicon]` table adds entries or
  replaces built-in ones: `roles = [{ word = "worship leader", phrase = "a
  person singing", needs = "action" }]` (`needs`, `action` or `object`, is
  optional; an action draws the notice unless the detector sees actions),
  `shots = [{ phrase = "upper body", sizes = ["medium"] }]`,
  `person_words = ["organist"]`.

## The ONNX detector

Real footage needs the OWLv2 open-vocabulary detector, which runs on ONNX
Runtime and is behind the `onnx` feature (off by default, so the default
build has no native dependencies):

```sh
cargo run -p auto-camera --features onnx -- --config config/file-source.example.toml
```

The `ort` crate downloads ONNX Runtime at build time, or links a local one
(`ORT_LIB_LOCATION`, with `ORT_PREFER_DYNAMIC_LINK=1` for a shared
library). Exporting the model, the expected files and CoreML on
Apple Silicon are described in [`crates/ac-vision/README.md`](crates/ac-vision/README.md).
Without the feature, a config with `kind = "owl"` fails at start with a
message saying so. Detections are whole people: a prompt picks the people
holding the object it names, so "a person playing guitar" frames guitarists,
not guitars (see "Prompts" in the crate README).

## The SAM 3.1 sidecar

SAM 3.1 finds people by what they do ("a person speaking"), which OWLv2
cannot express. On a Mac it runs through mlx-vlm in Python, so it runs as a
separate process, a sidecar, behind the same detector interface
([ADR 0004](docs/adr/0004-out-of-process-model-hosts.md)). One process
serves every source. `kind = "sidecar"` makes it the studio's detector, and
every query goes to it
([`config/sam3-sidecar.example.toml`](config/sam3-sidecar.example.toml)):

```toml
[detector]
kind = "sidecar"
command = ["../models/sam3/.venv/bin/python", "../sidecars/sam3-mlx/server.py",
           "--backend", "mlx", "--model", "../models/sam31-mlx", "--side", "672"]
score_threshold = 0.3        # default 0.5; SAM's scores are not OWLv2's
# send_side = 1008           # longest side sent, unless the shorter would go below --side
# startup_timeout_s = 60     # wait for the model to load
# request_timeout_s = 5      # one pass; must be below discovery.timeout_s

[discovery]
rate_hz = 1.0
max_result_age_s = 3.0       # answers are one pass old; room for slower passes
```

```sh
cargo run -p auto-camera -- --config config/sam3-sidecar.example.toml
```

- `command` is a program and its arguments, never a shell line. Elements
  with a `/` that do not start with `-` or `~` are paths, resolved against
  the config file; a bare program name is looked up on `PATH`. The program
  and every such path must exist, and the process must start, or the
  studio does not start and says why. `--check` finds a missing program,
  venv or path without starting the studio or the sidecar.
- The model loads while the sources play (4.6 s on Noah's M3 Max, then a
  warm-up pass). Until the sidecar's first start ends, with its hello or
  a failure, every source is preparing: no frame is sent and no timeout
  counted, the window says "preparing the detector", and a headless run
  waits for it (for at least `startup_timeout_s`), but not for a restart.
  A sidecar that fails to start (a wrong `--model` is
  reported in milliseconds, a cold load can miss `startup_timeout_s`), or
  that crashes or misses a deadline later, is restarted after a backoff of
  1 s, doubling up to 30 s; meanwhile its passes fail with the reason,
  shown in amber in the window. Quitting, by closing the window or with
  Ctrl-C, closes its stdin, waits up to 2 s, then kills and reaps it, and
  the sidecar exits by itself if the app dies: no process is left behind.
- Sources take turns once the sidecar is ready: one frame is with it at a
  time, and the source that waited longest goes next with its newest frame. Answers are
  then about one pass old whatever the number of sources, so the 2 s
  `max_result_age_s` default fits; each source gets a pass about every
  N × the pass time, and `max_undetected_s` should cover about three of
  those intervals.
- Measured at 672 px: 0.48 s per pass for 1 prompt, 0.64 s for 3, 1.05 s
  for 9. "a person speaking" found the speaker in 10 of 20 labelled frames
  at a 0.5 threshold and 14 of 20 at 0.3. The subject store's floor follows
  a `score_threshold` below its own 0.35.
- The venv, the weights, the licence (Meta's SAM License) and the
  protocol are in [`sidecars/sam3-mlx/README.md`](sidecars/sam3-mlx/README.md)
  and [`crates/ac-sidecar/README.md`](crates/ac-sidecar/README.md). The
  real model has not run through this path yet; on the Mac, run the smoke
  test described there first.

This is Phase 2 of the model-picker design: one detector for the studio.
Every prompt gets SAM's false hits too, so it is for trying prompts rather
than for a show; choosing a model per query comes later.

## Program loudness

While the output runs with program audio (`output.audio_source`), the
paced audio the encoder gets is also metered after ITU-R BS.1770-4 and
EBU R 128 (`crates/ac-audio`): momentary (400 ms) and short-term (3 s)
loudness, integrated loudness over the session (gated at −70 LUFS and
10 LU below the programme, so silence and pauses do not count), and the
session's sample and 4× oversampled true peak. The output panel shows
momentary and short-term as bars, integrated loudness and the true peak as
numbers (amber above −1 dBTP), and "—" where there is no reading yet or
the program is silent. A headless run reports the session's values in
`output.loudness` (`integrated_lufs`, `momentary_lufs`, `short_term_lufs`,
`sample_peak_dbfs`, `true_peak_dbtp`, `null` without a reading, and the
`seconds` of audio measured).

## Crates

| Crate | Role |
|---|---|
| `ac-core` | Domain model (sources, queries, subjects, presets, shots), the prompt parser and its lexicon (shot and role words), and normalized geometry |
| `ac-media` | Frames, audio blocks, the source and audio input traits, latest-frame mailboxes and audio rings, and the stereo channel pick |
| `ac-audio` | Program loudness and peak meter (BS.1770-4, EBU R 128): K-weighting, momentary, short-term and gated integrated loudness, sample and true peak; drift correction of audio inputs (drift estimator, band-limited resampler) |
| `ac-sources` | File (ffmpeg), synthetic and capture (avfoundation, by unique ID) sources, file segments, audio-only file inputs (WAV, RF64), CoreAudio device inputs by UID (feature `coreaudio`) and a fake input that drifts and stalls, a frame-by-frame clip reader for offline tools; injectable clocks |
| `ac-vision` | Detector and tracker traits, template tracker, OWLv2 ONNX detector, replay of recorded detections |
| `ac-sidecar` | Out-of-process detector host (SAM 3.1 on MLX): protocol v1 over a child's pipes, deadlines, restarts, one process shared by every source |
| `ac-subjects` | Per-source subject store: identities from detections and tracks |
| `ac-framing` | Ideal crops (fill rule for objects, body cut lines for whole people, zoom-limit flags) and per-shot smoothing |
| `ac-director` | Shot scoring, suggestions and opt-in auto-takes |
| `ac-switcher` | Preview/program, cut/auto/T-bar transitions, the take queued during one |
| `ac-compositor` | CPU program compositor |
| `ac-output` | One ffmpeg encode to RTMP and fragmented MP4; `caffeinate` while live |
| `ac-eval` | Evaluation-set label files (versioned JSON schema, validator with field paths, MOT import), the per-tick trace schema (`TraceRecord`) the engine emits, and the framing, motion, tracking and switching scorers that measure a trace against the labels, judged by a TOML thresholds file into a JSON report |
| `ac-engine` | The runtime that wires them together (ADR 0002), the studio config and its readiness check |
| `apps/auto-camera` | Operator window, headless runner, `--check` and `--list-audio-devices` |
| `apps/eval-runner` | The engine stepped over a clip on simulated time, with live or replayed detections, writing the trace |

## Develop

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test                                   # no GPU, camera or network needed;
                                             # ffmpeg tests skip without ffmpeg
```

Set `AC_REQUIRE_FFMPEG=1` to make the ffmpeg tests fail instead of skip when
ffmpeg is missing; CI does, after installing it. The sidecar host's tests
also drive `sidecars/sam3-mlx/server.py` on its fake backend when
`python3` (3.10 or newer) is on `PATH`, and skip otherwise. The sidecar's
own tests need only the standard library:

```sh
(cd sidecars/sam3-mlx && python3 -m unittest)
```

No Rust module may be over 600 lines unless it is a test module (a
`tests.rs`, a file under a `tests` directory, or a module declared with
`#[cfg(test)]`); CI checks it with a standard-library script and its tests:

```sh
python3 -B scripts/module_size.py            # lists any module over budget
python3 -B -m unittest discover -s scripts   # -B: no __pycache__ in the tree
```

CI also lints the sidecar with Ruff 0.16.8 (`ruff check` and `ruff format
--check` in `sidecars/sam3-mlx`), builds the app with `--release` and
records three seconds of the headless demo, and checks the supply chain.
cargo-deny holds every crate to [`deny.toml`](deny.toml): RustSec
advisories, and licences, where AGPL, GPL and LGPL crates are denied (a
probe package that depends on an AGPL crate must fail). The parts that are
not crates (ffmpeg, macOS's `caffeinate` and `system_profiler`, ONNX
Runtime, model weights, the sidecar's interpreter and Python packages) have
their licences in [`docs/licence-manifest.md`](docs/licence-manifest.md),
and a standard-library script fails when one the repository uses has no
entry:

```sh
python3 -B scripts/licence_manifest.py       # lists any component without an entry
```

The workflow's actions are pinned to commit SHAs; its header says how to
update one.
