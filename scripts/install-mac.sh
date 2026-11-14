#!/usr/bin/env bash
# One-shot installer for Auto Camera on an Apple Silicon Mac.
#
# Run it from a fresh clone (docs/install-mac.md walks through it):
#
#   git clone https://github.com/NoahIsCool/auto-camera.git ~/repos/auto-camera
#   cd ~/repos/auto-camera && scripts/install-mac.sh
#
# Steps, each skipped when its result is already there, so a second run only
# redoes what is missing or failed:
#
#   1. checks: macOS on arm64, Xcode Command Line Tools, Homebrew, memory
#   2. tools: ffmpeg and Python 3.12 from Homebrew, Rust through rustup
#   3. OWLv2: a venv in models/.venv, exported to models/owlv2-split-onnx
#   4. SAM 3.1: a venv in models/sam3/.venv and the pinned MLX weights in
#      models/sam31-mlx, checked against their SHA-256
#   5. build: target/release/auto-camera with the onnx and coreaudio features
#   6. warm-up: one headless run on a generated test clip, so the 2 to 3
#      minute CoreML compile happens now and not when the show starts
#
# Everything the script downloads or builds stays in this checkout (models/
# and target/ are gitignored) except Homebrew packages and the Rust toolchain.
# It never uses sudo. Options:
#
#   --skip-sam      no SAM 3.1 venv or weights (OWLv2 alone)
#   --skip-warmup   do not run the app once after building
#   --rust VERSION  Rust toolchain to install and build with (default 1.97)
#   -h, --help      this text

set -euo pipefail

RUST_VERSION="1.97"
SKIP_SAM=0
SKIP_WARMUP=0

# SAM 3.1, MLX conversion; the same pins as the weights measured in
# research/sam31-vs-owlv2.md (old prototype's scripts/prepare_sam31.py).
SAM_REPO="mlx-community/sam3.1-bf16"
SAM_REVISION="a992e302ea9b0f03f41dfd93414a4fd0e818f65b"
SAM_WEIGHT_SHA256="a1b1c19dcc9bdd68438bcd74433fadc90740e73c37a1f386872672d134879c42"
CLIP_TOKENIZER="openai/clip-vit-base-patch32"
CLIP_REVISION="3d74acf9a28c67741b2f4f2ea7635f0aaf6f0268"
OWL_MODEL="google/owlv2-base-patch16-ensemble"

usage() { sed -n '2,/^set -euo/p' "$0" | sed -e '/^set -euo/d' -e 's/^# \{0,1\}//'; }

while [ $# -gt 0 ]; do
  case "$1" in
    --skip-sam) SKIP_SAM=1 ;;
    --skip-warmup) SKIP_WARMUP=1 ;;
    --rust) RUST_VERSION="${2:?--rust needs a version}"; shift ;;
    -h | --help) usage; exit 0 ;;
    *) echo "unknown option: $1 (see --help)" >&2; exit 2 ;;
  esac
  shift
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

step() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
info() { printf '    %s\n' "$*"; }
fail() { printf '\n\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------- 1. checks
step "Checking this Mac"
[ "$(uname -s)" = "Darwin" ] || fail "this installer is for macOS"
[ "$(uname -m)" = "arm64" ] || fail "this installer needs an Apple Silicon Mac (arm64)"
[ -f Cargo.toml ] && [ -d apps/auto-camera ] || fail "run it from an auto-camera checkout"
info "macOS $(sw_vers -productVersion), $(sysctl -n machdep.cpu.brand_string)"
MEM_GB=$(( $(sysctl -n hw.memsize) / 1024 / 1024 / 1024 ))
info "memory: ${MEM_GB} GB"
if [ "$MEM_GB" -lt 16 ]; then
  info "warning: under 16 GB. The first CoreML compile of OWLv2 peaks near 7 GB"
  info "and SAM 3.1 needs about 3.3 GB more; close other apps for the warm-up."
fi

if ! xcode-select -p >/dev/null 2>&1; then
  xcode-select --install || true
  fail "install the Xcode Command Line Tools (a dialog just opened), then run this again"
fi
info "Xcode Command Line Tools: $(xcode-select -p)"

BREW=""
for candidate in /opt/homebrew/bin/brew "$(command -v brew 2>/dev/null || true)"; do
  if [ -n "$candidate" ] && [ -x "$candidate" ]; then BREW="$candidate"; break; fi
done
if [ -z "$BREW" ]; then
  fail "Homebrew is missing. Install it from https://brew.sh (it asks for your password), then run this again"
fi
eval "$("$BREW" shellenv)"
info "Homebrew: $("$BREW" --prefix)"

# ----------------------------------------------------------------- 2. tools
step "Installing ffmpeg and Python 3.12"
for formula in ffmpeg python@3.12; do
  if "$BREW" list --versions "$formula" >/dev/null 2>&1; then
    info "$formula already installed: $("$BREW" list --versions "$formula")"
  else
    "$BREW" install "$formula"
  fi
done
PYTHON="$("$BREW" --prefix python@3.12)/bin/python3.12"
[ -x "$PYTHON" ] || fail "python3.12 not found at $PYTHON"
info "ffmpeg: $(ffmpeg -hide_banner -version | head -n 1)"

step "Installing Rust $RUST_VERSION"
export PATH="$HOME/.cargo/bin:$PATH"
if ! command -v rustup >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
    sh -s -- -y --profile minimal --default-toolchain "$RUST_VERSION" --no-modify-path
fi
rustup toolchain install "$RUST_VERSION" --profile minimal
info "$(cargo +"$RUST_VERSION" --version)"

# ----------------------------------------------------------------- 3. OWLv2
step "OWLv2 detector (ONNX export)"
OWL_DIR="models/owlv2-split-onnx"
if [ -f "$OWL_DIR/detector.onnx" ] && [ -f "$OWL_DIR/text_encoder.onnx" ] && [ -f "$OWL_DIR/tokenizer.json" ]; then
  info "already exported in $OWL_DIR"
else
  mkdir -p models
  [ -x models/.venv/bin/python ] || "$PYTHON" -m venv models/.venv
  models/.venv/bin/pip install --quiet --upgrade pip
  # The versions the export was checked with (crates/ac-vision/README.md).
  models/.venv/bin/pip install --quiet "torch~=2.14.0" "transformers~=4.57.0" onnx onnxruntime pillow
  info "downloading $OWL_MODEL (about 600 MB) and exporting; a few minutes"
  HF_HOME="$ROOT/models/hf" models/.venv/bin/python \
    crates/ac-vision/scripts/export_owlv2_split.py "$OWL_MODEL" "$OWL_DIR.partial"
  rm -rf "$OWL_DIR"
  mv "$OWL_DIR.partial" "$OWL_DIR"
  info "exported to $OWL_DIR"
fi

# --------------------------------------------------------------- 4. SAM 3.1
if [ "$SKIP_SAM" = 1 ]; then
  step "SAM 3.1 sidecar: skipped (--skip-sam)"
else
  step "SAM 3.1 sidecar (Python venv and MLX weights)"
  SAM_VENV="models/sam3/.venv"
  if [ -x "$SAM_VENV/bin/python" ] && "$SAM_VENV/bin/python" -c "import mlx_vlm" 2>/dev/null; then
    info "venv already set up in $SAM_VENV"
  else
    mkdir -p models/sam3
    [ -x "$SAM_VENV/bin/python" ] || "$PYTHON" -m venv "$SAM_VENV"
    "$SAM_VENV/bin/pip" install --quiet --upgrade pip
    "$SAM_VENV/bin/pip" install --quiet -r sidecars/sam3-mlx/requirements.txt
    info "venv ready in $SAM_VENV"
  fi

  SAM_DIR="models/sam31-mlx"
  if [ -L "$SAM_DIR" ]; then
    info "$SAM_DIR is a link to $(readlink "$SAM_DIR"); left as it is"
  else
    mkdir -p "$SAM_DIR"
    base="https://huggingface.co/$SAM_REPO/resolve/$SAM_REVISION"
    for name in config.json processor_config.json model.safetensors; do
      [ -f "$SAM_DIR/$name" ] && continue
      info "downloading $name"
      if ! curl --fail --location --retry 3 --silent --show-error \
        --output "$SAM_DIR/$name.partial" "$base/$name"; then
        rm -f "$SAM_DIR/$name.partial"
        # Only the weights and their config are required; the processor
        # config is optional in mlx-vlm's loader.
        [ "$name" = processor_config.json ] && { info "(no $name upstream; fine)"; continue; }
        fail "download of $name failed"
      fi
      mv "$SAM_DIR/$name.partial" "$SAM_DIR/$name"
    done
    info "checking the weights' SHA-256"
    actual="$(shasum -a 256 "$SAM_DIR/model.safetensors" | cut -d ' ' -f 1)"
    if [ "$actual" != "$SAM_WEIGHT_SHA256" ]; then
      mv "$SAM_DIR/model.safetensors" "$SAM_DIR/model.safetensors.bad"
      fail "model.safetensors has SHA-256 $actual, expected $SAM_WEIGHT_SHA256 (kept as model.safetensors.bad)"
    fi
    if [ ! -d "$SAM_DIR/tokenizer" ]; then
      info "saving the CLIP tokenizer"
      HF_HOME="$ROOT/models/hf" "$SAM_VENV/bin/python" -c \
        "import sys; from transformers import CLIPTokenizer; CLIPTokenizer.from_pretrained(sys.argv[1], revision=sys.argv[2]).save_pretrained(sys.argv[3])" \
        "$CLIP_TOKENIZER" "$CLIP_REVISION" "$SAM_DIR/tokenizer"
    fi
    printf '{\n  "model": "%s",\n  "revision": "%s",\n  "weight_sha256": "%s"\n}\n' \
      "$SAM_REPO" "$SAM_REVISION" "$SAM_WEIGHT_SHA256" >"$SAM_DIR/provenance.json"
    info "weights ready in $SAM_DIR"
  fi
fi

# ----------------------------------------------------------------- 5. build
step "Building auto-camera (release, onnx + coreaudio); about 5 minutes the first time"
cargo +"$RUST_VERSION" build --release --locked -p auto-camera --features onnx,coreaudio
APP="$ROOT/target/release/auto-camera"
# The example configs record to recordings/ under the working folder, and
# the app does not create it.
mkdir -p recordings
info "built $APP"

# --------------------------------------------------------------- 6. warm-up
if [ "$SKIP_WARMUP" = 1 ]; then
  step "Warm-up: skipped (--skip-warmup); the first OWLv2 run will spend 2 to 3 minutes preparing"
else
  step "Warm-up: compiling OWLv2 for CoreML (2 to 3 minutes the first time; do not interrupt)"
  WARM="models/warmup"
  mkdir -p "$WARM"
  if [ ! -f "$WARM/clip.mp4" ]; then
    ffmpeg -hide_banner -loglevel error -y \
      -f lavfi -i testsrc2=size=1280x720:rate=30 -f lavfi -i sine=frequency=440 \
      -t 10 -c:v libx264 -pix_fmt yuv420p -c:a aac "$WARM/clip.mp4"
  fi
  # The file-source example, pointed at the clip and at the models folder.
  sed -e 's#"\.\./media/rehearsal\.mp4"#"clip.mp4"#' \
    -e 's#"\.\./models/#"../#g' \
    -e 's#^record_path = .*#record_path = "models/warmup/warmup.mp4"#' \
    config/file-source.example.toml >"$WARM/warmup.toml"
  "$APP" --config "$WARM/warmup.toml" --check
  "$APP" --config "$WARM/warmup.toml" --headless --seconds 5 >"$WARM/summary.json"
  info "warm-up done; summary in $WARM/summary.json"
fi

# ------------------------------------------------------------------- done
step "Done"
cat <<EOF
    Start the operator window on the demo studio (no camera or model needed):
      $APP

    With OWLv2 on a video file (copy the example, set its path first):
      $APP --config config/file-source.example.toml

    With cameras:      config/capture.example.toml
    With SAM 3.1:      config/sam3-sidecar.example.toml
    Check any config:  $APP --config <file> --check

    See docs/install-mac.md for camera permission, the first run and updates.
EOF
