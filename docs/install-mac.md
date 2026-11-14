# Installing Auto Camera on a Mac

For a show machine such as an M3 Mac mini: any Apple Silicon Mac on a recent
macOS. One script installs the tools, sets up both detectors, builds the app
and runs it once so the slow first-time CoreML compile is out of the way.

## What you need

- An Apple Silicon Mac (M1 or newer). 16 GB of memory or more is
  recommended: the first CoreML compile of OWLv2 peaks near 7 GB, and the
  SAM 3.1 sidecar needs about 3.3 GB on top of the app while it runs.
  8 GB works for OWLv2 alone if nothing else is open.
- About 15 GB of free disk: the Rust build (about 4 GB), the OWLv2 export
  and its CoreML cache (about 3 GB), PyTorch for the export (about 2 GB),
  and SAM 3.1's weights and venv (about 4 GB).
- An administrator account, for Homebrew and the Xcode Command Line Tools.
- A network connection for the install. The show itself runs offline except
  for the RTMP stream.

## Install

```sh
xcode-select --install          # if the Command Line Tools are not there yet
# Homebrew, if missing: follow https://brew.sh
git clone https://github.com/NoahIsCool/auto-camera.git ~/repos/auto-camera
cd ~/repos/auto-camera
scripts/install-mac.sh
```

It takes 15 to 30 minutes, most of it downloads, the Rust build and the
CoreML compile. Leave it running until it prints **Done**: the warm-up at
the end is the CoreML compile, and stopping it part way means the first
real run pays it again. The script can be run again at any time; it skips
whatever is already there.

What it does, in order:

| Step | Result | Where |
|---|---|---|
| Checks | macOS on arm64, Command Line Tools, Homebrew, memory | |
| Tools | `ffmpeg`, Python 3.12 (Homebrew), Rust 1.97 (rustup) | Homebrew, `~/.cargo`, `~/.rustup` |
| OWLv2 | `google/owlv2-base-patch16-ensemble` exported to ONNX | `models/.venv`, `models/hf`, `models/owlv2-split-onnx` |
| SAM 3.1 | sidecar venv, MLX weights checked against their SHA-256, CLIP tokenizer | `models/sam3/.venv`, `models/sam31-mlx` |
| Build | `cargo build --release --features onnx,coreaudio` | `target/release/auto-camera` |
| Warm-up | a 5 s headless run on a generated test clip | `models/warmup`, CoreML caches |

Options: `--skip-sam` leaves out SAM 3.1 (OWLv2 alone), `--skip-warmup`
skips the warm-up run, `--rust VERSION` builds with another toolchain
(1.92 is the oldest the workspace accepts).

SAM 3.1's weights are under Meta's SAM License; read
[`sidecars/sam3-mlx/SAM_LICENSE.txt`](../sidecars/sam3-mlx/SAM_LICENSE.txt)
before using them.

## First run

```sh
target/release/auto-camera                     # demo studio: no camera or model
```

The demo studio is a synthetic camera with three moving subjects, so it
shows the window, the switcher and the director without any setup. For real
sources, copy one of the example configs and edit it:

| Config | For |
|---|---|
| [`config/file-source.example.toml`](../config/file-source.example.toml) | a video file, OWLv2 |
| [`config/capture.example.toml`](../config/capture.example.toml) | cameras and capture cards |
| [`config/sam3-sidecar.example.toml`](../config/sam3-sidecar.example.toml) | a video file, SAM 3.1 |

```sh
cp config/capture.example.toml config/show.toml   # then edit it
target/release/auto-camera --config config/show.toml --check
target/release/auto-camera --config config/show.toml
```

`--check` lists every missing file, tool or folder without starting
anything, and exits 2 when there is a problem. `--list-audio-devices` lists
the board's CoreAudio inputs and their UIDs for `kind = "device"` audio
inputs. The README's "Studio configuration", "Cameras and capture devices"
and "Readiness check" sections cover the rest.

## macOS permissions

macOS asks once per app, the first time it is needed. Allow each one, or
grant it in System Settings, Privacy & Security:

- **Camera**, for capture sources: the request names the app you start
  Auto Camera from (Terminal, iTerm), because ffmpeg reads the cameras.
  Without it ffmpeg cannot open the camera and the source keeps retrying,
  with ffmpeg's error in the window and the log.
- **Microphone**, for the board's audio input (`kind = "device"`).
- Keep the Mac from sleeping during a show: Auto Camera holds it awake
  while program goes out (README, "Staying awake while live"). Do not
  minimize the window during a show: App Nap can throttle a minimized
  window, and there is no app bundle yet to turn that off.

## Updating

```sh
cd ~/repos/auto-camera
git pull
scripts/install-mac.sh --skip-warmup     # rebuilds; models are kept
target/release/auto-camera --config config/show.toml --check
```

Each new build pays part of the CoreML compile again (about 2 minutes, while
the window shows `preparing detector m:ss`), because macOS keeps that cache
per executable. Run the warm-up again (`scripts/install-mac.sh` without
`--skip-warmup`) or start the app once before the show and wait for the
detector to be ready.

## When something goes wrong

- **"Error compiling model"** when OWLv2 loads: a first compile was
  interrupted. Delete `models/owlv2-split-onnx/coreml-cache` and start
  again.
- **SHA-256 mismatch** for `model.safetensors`: the download was corrupted
  or the upstream file changed. The bad file is kept as
  `model.safetensors.bad`; delete it and run the script again.
- **The SAM sidecar does not start**: run its smoke test, described in
  [`sidecars/sam3-mlx/README.md`](../sidecars/sam3-mlx/README.md).
- **Logs and crash reports** are in `~/Library/Logs/auto-camera`.
