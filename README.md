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
recording path, start/stop, encoder statistics). Under the prompt a live
line says how it reads, for example "Detector sees: a person speaking ·
Framing: Medium, Medium close (person)", and the framing checkboxes follow
the prompt's shot words until you tick one yourself (see
[Prompts](#prompts)). A tile whose preset's zoom ceiling keeps it looser
than its size carries an amber LIMITED badge in the corner of its picture,
and the monitors and the director's suggestion name it "Speaker 1 Close
(limited)"; its hover text says when it looks the same as another of the
subject's shots (two sizes both drawn at the ceiling). The status bar shows
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
the first wide is cut to program), waits up to five minutes for the
detector to load (a CoreML compile can take minutes; the run fails if it is
still loading then), starts recording when `--record` is given
(the stream also goes to `output.rtmp_url` if the config sets one), runs for
`--seconds` (default 10) after recording started, finalizes the recording,
shuts down and prints a JSON summary on stdout. Besides counters, each
source says whether its detector was still preparing and when it was ready
(`discovery_preparing`, `detector_ready_s`), `queries` says how each
prompt was read, each shot gives the range of
its crop size over the run (`crop_width_min` to `crop_height_max`), and
`subjects` lists the subjects at the end with the queries they answer:

```json
{
  "ok": true,
  "ticks": 160,
  "sources": [{ "id": "stage", "fps": 31.0, "frames": 155, "missed": 3, "discovery_preparing": false, "detector_ready_s": 0.03, "discovery_applied": 40 }],
  "queries": [{ "prompt": "person", "detector_phrase": "person", "presets": ["medium", "close"], "basis": "person",
                "parse": "Detector sees: person · Framing: no shot words (person)" }],
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

[[queries]]
name = "Drummer"
prompt = "drummer wide"        # no presets: the shot word "wide" picks Full, and
                               # the detector is asked for "a person playing drums"

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
times), `framing`, `switcher` (default transition), `monitors` (monitor
and multiview tile sizes and rates) and `lexicon` (below); see
`crates/ac-engine/src/config.rs`.

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
  person singing" }]`, `shots = [{ phrase = "upper body", sizes =
  ["medium"] }]`, `person_words = ["organist"]`.

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
| `ac-core` | Domain model (sources, queries, subjects, presets, shots), the prompt parser and its lexicon (shot and role words), and normalized geometry |
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
