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

## Demo

Requires Rust 1.92 or newer, and `ffmpeg` for file sources and output:
`AC_FFMPEG` / `AC_FFPROBE` name the executables, else they are looked up on
`PATH`, and on macOS also in `/opt/homebrew/bin` and `/usr/local/bin` (an
app started from Finder or the Dock gets a `PATH` without Homebrew). The
demo studio ([`config/demo.toml`](config/demo.toml)) is one synthetic 720p
camera with three moving subjects and a ground-truth detector, so it needs
no camera, model or video file. Its director starts in Suggest mode: it
proposes shots and the operator takes them.

```sh
cargo run -p auto-camera                     # operator window on the demo studio
```

The window has the preview (green) and program (red) monitors with Cut,
Auto, the transition selector and a T-bar between them; under them the
multiview of every shot with tally borders, tracking status and quality.
On the right: the director (mode Off/Suggest/Auto, profile Talk/Music, the
current suggestion and why, Accept), the queries (add a prompt with its
framings, remove one; pin, hide or show subjects) and the output (RTMP URL,
recording path, start/stop, encoder statistics). The status bar shows
source frame rates, tick timing and dropped frames.

| Input | Action |
|---|---|
| click a tile | stage it on preview |
| double-click a tile | stage it and cut it to program |
| right-click a subject tile | pin, hide or re-acquire the subject |
| `1`–`9` | stage tile 1–9 on preview |
| `Space` | cut |
| `Enter` | auto transition |

Shortcuts are ignored while a text field has focus and on key repeat.

## Headless mode

```sh
cargo run -p auto-camera -- --headless --seconds 5 --record /tmp/demo.mp4
auto-camera [--config PATH] --headless [--seconds N] [--record PATH]
```

A headless run starts the engine without a window, waits for the first
picture, puts a shot on air (the director does it in Auto mode, otherwise
the first wide is cut to program), starts recording when `--record` is given
(the stream also goes to `output.rtmp_url` if the config sets one), runs for
`--seconds` (default 10) after recording started, finalizes the recording,
shuts down and prints a JSON summary on stdout:

```json
{
  "ok": true,
  "ticks": 160,
  "sources": [{ "id": "stage", "fps": 31.0, "frames": 155, "missed": 3, "discovery_applied": 40 }],
  "max_subjects": 3,
  "program": { "last": "stage/wide", "changes": 1, "log": [[0.23, "stage/wide"]] },
  "director": { "mode": "suggest", "profile": "talk", "takes": 0, "reason": "best available shot" },
  "timing": { "budget_ms": 33.33, "avg_ms": 6.52, "max_ms": 19.44, "late_ticks": 0, "skipped_ticks": 0 },
  "output": { "state": "finished", "frames_written": 151, "frames_dropped": 0, "encoder": "libx264", "exit_code": 0 }
}
```

(abridged). The exit status is 0 on success, 1 when the engine or the output
failed (`ok` is false and `error` says why), 2 for usage and configuration
errors. Recordings are never overwritten: an existing `--record` path is an
error.

## Studio configuration

A studio is one TOML file (or JSON, by extension). Every section but
`sources` is optional, unknown keys are errors, and relative paths are
resolved against the config file's directory. A studio fed by a local video
file ([`config/file-source.example.toml`](config/file-source.example.toml)):

```toml
[[sources]]
id = "cam-1"
name = "Rehearsal"
kind = "file"                # or "synthetic" with width, height, fps, subjects
path = "../media/rehearsal.mp4"
loop_playback = true
max_width = 1280
audio = true

[[queries]]
name = "Speaker"
prompt = "a person speaking at a lectern"
presets = ["medium", "close"]  # wide is always there; also full, medium_close,
                               # or { id, name, size, headroom, max_zoom }

[detector]
kind = "owl"                 # "synthetic" (ground truth) or "none"
model = "../models/owlv2-base-onnx/model.onnx"
tokenizer = "../models/owlv2-base-onnx/tokenizer.json"

[discovery]
rate_hz = 1.0                # detector passes per second per source

[director]
mode = "suggest"             # off, suggest or auto
profile = "music"            # talk or music

[output]
width = 1280
height = 720
fps = 30
audio_source = "cam-1"       # program audio from this source
# rtmp_url = "rtmp://live.example.com/app/STREAM-KEY"
record_path = "../recordings/show.mp4"
```

Further sections tune the `tracker`, `subjects` (limits and lost/retire
times), `framing`, `switcher` (default transition) and `monitors` (monitor
and multiview tile sizes and rates); see `crates/ac-engine/src/config.rs`.

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

## Crates

| Crate | Role |
|---|---|
| `ac-core` | Domain model (sources, queries, subjects, presets, shots) and normalized geometry |
| `ac-media` | Frames, audio blocks and the source trait, with latest-frame mailboxes |
| `ac-sources` | File (ffmpeg) and synthetic sources; injectable clocks |
| `ac-vision` | Detector and tracker traits, template tracker, OWLv2 ONNX detector |
| `ac-subjects` | Per-source subject store: identities from detections and tracks |
| `ac-framing` | Ideal crops (fill rule for objects, body cut lines for whole people, zoom-limit flags) and per-shot smoothing |
| `ac-director` | Shot scoring, suggestions and opt-in auto-takes |
| `ac-switcher` | Preview/program, cut/auto/T-bar transitions |
| `ac-compositor` | CPU program compositor |
| `ac-output` | One ffmpeg encode to RTMP and fragmented MP4 |
| `ac-eval` | Framing-quality metrics |
| `ac-engine` | The runtime that wires them together (ADR 0002) |
| `apps/auto-camera` | Operator window and headless runner |

## Develop

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test                                   # no GPU, camera or network needed;
                                             # ffmpeg tests skip without ffmpeg
```

Set `AC_REQUIRE_FFMPEG=1` to make the ffmpeg tests fail instead of skip when
ffmpeg is missing; CI does, after installing it.
