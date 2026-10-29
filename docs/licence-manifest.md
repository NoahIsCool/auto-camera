# Licence manifest: the parts that are not crates

cargo-deny checks the licence of every crate the app is built from
([`deny.toml`](../deny.toml)) and denies AGPL, GPL and LGPL crates. This
file covers what it cannot see: the programs, native library, model
weights and Python packages that Auto Camera runs, links or loads but that
do not come from crates.io. [`scripts/licence_manifest.py`](../scripts/licence_manifest.py)
finds them in the repository (its docs give the rules), and CI fails when
one has no row in the table below.

None of them is in this repository, and there is no release bundle yet:
each comes from where its section says. The obligations below are what a
bundle that includes the part would have to meet. This is a summary, not
legal advice; each licence text is the source.

Each row was checked on 2026-10-28 against the licence source it names.
**yes**: the licence was read there; **partly**: some of it could not be
read, and the section says what; **no**: not checked. A new row needs an
id in backquotes (the check prints the ids it is missing), a licence, an
`https://` link to the licence text, and `yes`, `partly` or `no`.

| Id | Component | Licence | Licence source | Checked |
|---|---|---|---|---|
| `native:ffmpeg` | FFmpeg: the `ffmpeg` and `ffprobe` programs | LGPL-2.1-or-later; GPL-2.0-or-later when built with `--enable-gpl` (libx264 needs it); GPL-3.0-or-later with `--enable-version3` too | [FFmpeg `LICENSE.md`](https://github.com/FFmpeg/FFmpeg/blob/master/LICENSE.md) | yes |
| `native:caffeinate` | `caffeinate`, the macOS tool the app runs as `/usr/bin/caffeinate` to keep the Mac awake while program goes out | Part of macOS, under Apple's licence for macOS; Apple publishes its source under APSL-2.0 | [`caffeinate.c`](https://github.com/apple-oss-distributions/PowerManagement/blob/main/caffeinate/caffeinate.c), [PowerManagement `APPLE_LICENSE`](https://github.com/apple-oss-distributions/PowerManagement/blob/main/APPLE_LICENSE) | partly |
| `native:system_profiler` | `system_profiler`, the macOS tool the app runs as `/usr/sbin/system_profiler` to list the connected cameras and their unique IDs before a capture source opens | Part of macOS, under Apple's licence for macOS | [Apple software licence agreements](https://www.apple.com/legal/sla/) | no |
| `native:onnxruntime` | ONNX Runtime 1.28.0, linked into the app with `--features onnx` | MIT, and the licences of the code it includes | [`LICENSE`](https://github.com/microsoft/onnxruntime/blob/v1.28.0/LICENSE), [`ThirdPartyNotices.txt`](https://github.com/microsoft/onnxruntime/blob/v1.28.0/ThirdPartyNotices.txt) | partly |
| `native:python` | CPython, the interpreter the SAM sidecar's `command` names (the user's own, 3.10 or newer, unpinned) | PSF-2.0; the code it includes under its own licences | [CPython `LICENSE`](https://github.com/python/cpython/blob/main/LICENSE), [incorporated software, `Doc/license.rst`](https://github.com/python/cpython/blob/main/Doc/license.rst) | yes |
| `model:owlv2-split-onnx` | OWLv2 base patch16 ensemble weights (Google), exported to ONNX, with their tokenizer | Apache-2.0 | [scenic OWL-ViT README, License](https://github.com/google-research/scenic/blob/main/scenic/projects/owl_vit/README.md#license), [`LICENSE`](https://github.com/google-research/scenic/blob/main/LICENSE) | partly |
| `model:sam31-mlx` | SAM 3.1 weights (Meta), converted to MLX, with their tokenizer | SAM License (Meta, last updated 19 November 2025) | [facebookresearch/sam3 `LICENSE`](https://github.com/facebookresearch/sam3/blob/main/LICENSE); copy in [`SAM_LICENSE.txt`](../sidecars/sam3-mlx/SAM_LICENSE.txt) | partly |
| `pypi:mlx` | MLX 0.32.3 (Apple) | MIT | [`LICENSE` at v0.32.3](https://github.com/ml-explore/mlx/blob/v0.32.3/LICENSE) | yes |
| `pypi:mlx-vlm` | mlx-vlm 0.7.6 at `5b22c24` | MIT | [`LICENSE` at 5b22c24](https://github.com/Blaizzy/mlx-vlm/blob/5b22c24f325a953ec70b965e39fb97a9b5ec7a4d/LICENSE) | yes |
| `pypi:transformers` | Transformers 5.18.0 (Hugging Face) | Apache-2.0 | [`LICENSE` at v5.18.0](https://github.com/huggingface/transformers/blob/v5.18.0/LICENSE) | yes |
| `pypi:numpy` | NumPy, unpinned (2.5.4 when checked) | BSD-3-Clause AND 0BSD AND MIT AND Zlib AND CC0-1.0 | [PyPI, numpy 2.5.4](https://pypi.org/project/numpy/2.5.4/), [`LICENSE.txt`](https://github.com/numpy/numpy/blob/main/LICENSE.txt) | yes |
| `pypi:opencv-python` | OpenCV wheels, unpinned (5.0.0.93 when checked) | Apache-2.0 (OpenCV) and MIT (the packaging); the wheels include FFmpeg under LGPL-2.1, the non-headless Linux wheels Qt 5 under LGPL-3.0 | [opencv-python README, Licensing](https://github.com/opencv/opencv-python/blob/5.x/README.md#licensing) | yes |
| `pypi:pillow` | Pillow, unpinned (13.0.0 when checked) | MIT-CMU | [PyPI, pillow 13.0.0](https://pypi.org/project/pillow/13.0.0/), [`LICENSE`](https://github.com/python-pillow/Pillow/blob/main/LICENSE) | yes |

## FFmpeg

**What and where from.** File sources decode with `ffmpeg` and probe with
`ffprobe`; the program encoder is one `ffmpeg` process (`ac-sources`,
`ac-output`). They are the programs named by `AC_FFMPEG` and `AC_FFPROBE`,
else the ones on `PATH` (and on macOS in Homebrew's folders): Homebrew's
`ffmpeg` on a Mac, the distribution's package on Linux. Auto Camera ships
neither.

**Which licence.** It depends on how the build was configured (FFmpeg's
`LICENSE.md`): LGPL-2.1-or-later by default, GPL-2.0-or-later with
`--enable-gpl`, which libx264 requires, and the version-3 forms with
`--enable-version3`. The encoder uses `h264_videotoolbox` on macOS when
it works and falls back to `libx264`, so a build without libx264 loses
that fallback. Checked builds:

- Homebrew's formula (homebrew-core `Formula/f/ffmpeg.rb`, ffmpeg 9.0.2)
  declares `license "GPL-3.0-or-later"` and configures `--enable-gpl
  --enable-version3 --enable-libx264`.
- Ubuntu 24.04's ffmpeg 6.1.1 (the CI runners', and where this was written)
  is configured with `--enable-gpl`, and `ffmpeg -L` prints
  GPL-2.0-or-later.

**Obligations.** The app does not link FFmpeg: it starts it as a separate
program and exchanges raw frames and streams with it over pipes, which the
FSF's [GPL FAQ](https://www.gnu.org/licenses/gpl-faq.html#MereAggregation)
counts as communication between separate programs as a rule, so FFmpeg's
licence should not extend to the app (the FAQ could not be reached from
where this was written, so that reading is from memory). A bundle that
ships an ffmpeg binary must meet that build's licence: include its licence
text, provide or offer its exact source and configure flags, and use a
build without `--enable-nonfree`, whose binaries may not be redistributed.

## caffeinate

**What and where from.** On macOS the app keeps the system awake while
program goes out, and for the whole of a headless run, by running
`caffeinate` (`ac-output`'s `keep_awake`): the copy macOS installs at
`/usr/bin/caffeinate`, by that path. It is part of the operating system;
Auto Camera ships none, and runs nothing in its place elsewhere.

**Licence.** The program comes with macOS, under Apple's licence for
macOS. Apple publishes its source in the PowerManagement project:
`caffeinate/caffeinate.c`, read on `main`, says that it is subject to the
Apple Public Source License 2.0, and the project's `APPLE_LICENSE` is that
licence (APSL-2.0).

**Not verified:** Apple's licence for macOS itself, and which
PowerManagement release the `caffeinate` of a given macOS version was
built from.

**Obligations.** None: the app runs the copy that comes with macOS as a
separate program, and neither ships, links nor changes it.

## system_profiler

**What and where from.** On macOS a capture source finds its camera by
unique ID, before every open, from the list `system_profiler
SPCameraDataType -json` prints (`ac-sources`' `SystemInventory`): the copy
macOS installs at `/usr/sbin/system_profiler`, run by that path. It is
part of the operating system; Auto Camera ships none, and runs nothing in
its place elsewhere (a capture source then stays offline, saying it needs
macOS).

**Licence.** The program comes with macOS, under Apple's licence for
macOS; Apple lists its software licence agreements at the link in the
table.

**Not verified:** that page could not be reached from where this was
written (2026-10-29), so the row is `no`, and whether Apple publishes the
program's source was not checked.

**Obligations.** None: the app runs the copy that comes with macOS as a
separate program and reads what it prints, and neither ships, links nor
changes it.

## ONNX Runtime

**What and where from.** With `--features onnx`, `ort` 2.0.0-rc.13 links
ONNX Runtime 1.28.0 into the app statically: its build script downloads a
prebuilt library from `cdn.pyke.io` (pyke's builds of Microsoft's release;
on Apple Silicon with the CoreML execution provider), checks it against
the SHA-256 in `ort-sys`'s `build/download/dist.tsv`, and links it with
`rustc-link-lib=static=onnxruntime`. `ORT_LIB_LOCATION` links a local build
instead (see `crates/ac-vision/README.md`). The default build has no ONNX
Runtime.

**Licence.** ONNX Runtime is MIT (Copyright (c) Microsoft Corporation).
`ThirdPartyNotices.txt` at the same tag lists the code it includes or
derives from, under its own licences: among them protobuf, abseil, onnx,
flatbuffers, re2 and Eigen, which is MPL-2.0.

**Not verified:** the pyke archives themselves (`cdn.pyke.io` cannot be
reached from where this was written), so it is not known whether they
carry `LICENSE` and `ThirdPartyNotices.txt`, nor which of the noticed
components the static library contains.

**Obligations.** A binary built with `onnx` must ship ONNX Runtime's
`LICENSE` and `ThirdPartyNotices.txt` (MIT asks for the notice in every
copy). Eigen's MPL-2.0 asks that recipients of the binary be told where its
source is; it does not extend to the app.

## Python

**What and where from.** The SAM sidecar is a Python program, and the
detector's `command` starts it with an interpreter: in the example config
`models/sam3/.venv/bin/python`, the one of the virtual environment the
user creates (Python 3.10 or newer, from python.org, Homebrew or another
build). Auto Camera ships none. CI runs the sidecar's tests with the
runner's `python3`.

**Licence.** CPython's `LICENSE`, read on `main`: "Python software and
documentation are licensed under the Python Software Foundation License
Version 2" (PSF-2.0); the file also keeps the BeOpen, CNRI and CWI
agreements of the releases Python derives from, and code in the
documentation is under 0BSD as well. `Doc/license.rst` lists software
incorporated under other licences, among them expat, libffi, zlib,
libmpdec and mimalloc, and OpenSSL (Apache-2.0 from OpenSSL 3.0), which
`hashlib` and `ssl` use when the build has it and the macOS and Windows
installers include.

**Not covered:** which libraries a given interpreter build links or
bundles (OpenSSL, SQLite, Tcl/Tk, readline and others), which depends on
where it came from.

**Obligations.** None while the user provides the interpreter. A bundle
that ships one must keep the PSF licence agreement and PSF's copyright
notice with it (clause 2 of the agreement), add a brief summary of the
changes to a modified Python (clause 3), and include the licences of what
that build incorporates.

## OWLv2 weights

**What and where from.** The OWLv2 detector (`[detector] kind = "owl"`)
loads `google/owlv2-base-patch16-ensemble` from Hugging Face, exported to
ONNX on the user's machine by `crates/ac-vision/scripts/export_owlv2_split.py`
(`models/owlv2-split-onnx/`: `text_encoder.onnx`, `detector.onnx`,
`tokenizer.json`) or by `optimum-cli` (`models/owlv2-base-onnx/`, the
combined layout). `tokenizer.json` is the CLIP tokenizer from the same
repository.

**Licence.** Apache-2.0: Google's scenic repository, which released OWLv2,
says "Both the code and the model checkpoints are licensed under the Apache
2.0 license", and its `LICENSE` is Apache-2.0.

**Not verified:** the Hugging Face model card of
`google/owlv2-base-patch16-ensemble`, which is what the exports download
(`huggingface.co` cannot be reached from where this was written), so its
licence tag was not read.

**Obligations.** A bundle with the ONNX files must include the Apache-2.0
text and say that the files are a conversion of Google's checkpoints.

## SAM 3.1 weights

**What and where from.** The SAM 3.1 sidecar (`[detector] kind =
"sidecar"`, `sidecars/sam3-mlx`) loads an MLX conversion of SAM 3.1 from
`models/sam31-mlx` (`config.json`, safetensors, `tokenizer/`). Meta
releases the SAM 3.1 checkpoints in the gated Hugging Face repository
`facebook/sam3.1`, as the facebookresearch/sam3 README says; the folder in
use is the old prototype's conversion.

**Licence.** Meta's SAM License, last updated 19 November 2025, a custom
licence (no SPDX id). `sidecars/sam3-mlx/SAM_LICENSE.txt` is byte for byte
the `LICENSE` of facebookresearch/sam3 on `main` (SHA-256
`4dea99bfaa016e21bc860d73f344236bd1e5c4977d1a9a8fd32f822b500ae1be`), and
the README of that repository, which releases the SAM 3.1 checkpoints,
says the project is licensed under it.
The sidecar's README summarizes its terms.

**Not verified:** the gated `facebook/sam3.1` model card, which needs a
login, and where the prototype's MLX conversion came from, which is not
recorded; its card states no licence of its own
(`research/sam31-vs-owlv2.md`).

**Obligations.** Anything that gives the weights, or a conversion of them,
to someone else must do so under the SAM License with a copy of it
(`SAM_LICENSE.txt`); its field-of-use bans and termination clause apply to
every user. The rebuild specification (section e) asks for a legal review
before the weights are redistributed.

## The sidecar's Python packages

**What and where from.** The sidecar's MLX backend runs in a virtual
environment the user creates from `sidecars/sam3-mlx/requirements.txt`
(`pip install -r`); nothing of it is shipped. MLX is the array framework,
mlx-vlm the SAM 3.1 port, Transformers the CLIP tokenizer, OpenCV the
connected components of the speckle rule, NumPy the arrays, and Pillow the
resize in mlx-vlm's preprocessing (and the smoke test's pictures). The
fake backend and the other tests use none of them.

**Licences.** Read from each project's licence file at the pinned version
where there is one, and from the PyPI metadata of the newest release for
the three that are not pinned (NumPy, opencv-python, Pillow), so a later
release pip picks may differ. The opencv-python README says every wheel
includes FFmpeg (LGPL-2.1) and the non-headless Linux wheels Qt 5
(LGPL-3.0); it names Qt for no other platform.

**Not covered:** the packages these pull in (huggingface-hub, tokenizers,
safetensors and others), which pip resolves and the table does not list,
and the libraries bundled inside the NumPy and Pillow wheels.

**Obligations.** None while the user installs the packages. A bundle that
ships the environment must include each package's licence and notices, and
for the LGPL parts of OpenCV's wheels (FFmpeg, Qt) keep them replaceable
shared libraries, as the wheels ship them.
