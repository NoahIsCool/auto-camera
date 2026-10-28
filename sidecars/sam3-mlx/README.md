# SAM 3.1 sidecar (MLX)

A small Python process that runs SAM 3.1 *image detection* through
[mlx-vlm](https://github.com/Blaizzy/mlx-vlm) on Apple Silicon, for the Rust
host (`crates/ac-sidecar`, detector kind `"sidecar"`). The host starts it with
stdin and stdout as pipes, waits for `hello`, then sends one frame and its text
phrases at a time; the sidecar answers with normalized boxes and scores. It is
rung R0 of the model-picker design (section 4.6): it is retired only if an
in-process ONNX build meets the R1 exit criteria.

Per request the MLX backend runs the ViT backbone once at 672 px, then one
prompt pass per distinct phrase, and keeps only detections at or above each
phrase's threshold. On Noah's M3 Max that measured 0.48 s for 1 prompt, 0.64 s
for 3 and 1.05 s for 9, with a 4.6 s load and a 3.3 GiB peak
(`research/sam31-vs-owlv2.md`). No masks, parts or video tracking.

## Licence: Meta's SAM License

SAM 3.1's weights are Meta "SAM Materials" under the **SAM License** (last
updated 19 November 2025). A verbatim copy is in
[`SAM_LICENSE.txt`](SAM_LICENSE.txt), taken from
[facebookresearch/sam3](https://github.com/facebookresearch/sam3/blob/main/LICENSE).
The weights are not in this repository; the code in this folder is Auto
Camera's own (MIT OR Apache-2.0).

- **Grant:** non-exclusive, worldwide, non-transferable, royalty-free licence
  to use, reproduce, distribute and modify. There is no non-commercial clause.
- **Redistribution must carry it:** anything that gives the weights (or a
  derivative, such as the MLX conversion) to someone else must ship under the
  agreement's terms with a copy of the agreement. A release bundle that
  includes the weights must include `SAM_LICENSE.txt`. Publications of
  research done with SAM must acknowledge it.
- **Prohibited uses:** anything subject to ITAR, or end uses barred by trade
  controls: military or warfare, nuclear industries or applications,
  espionage, and guns or illegal weapons. No reverse engineering of the SAM
  Materials. Use must follow applicable law, including privacy and
  trade-control law, and the licensee must not be a sanctions target.
- **Termination:** the licence ends for anyone who sues Meta claiming the SAM
  Materials infringe their intellectual property.

This is a summary, not legal advice; the agreement is the source.

## On Noah's Mac

The venv and the weights already exist:

- venv: `~/repos/auto-camera/models/sam3/` (mlx 0.32.3, mlx-vlm at revision
  `5b22c24f325a953ec70b965e39fb97a9b5ec7a4d`, transformers 5.18.0). The
  design's config calls its interpreter `models/sam3/.venv/bin/python`; if
  `models/sam3` is itself the venv, use `models/sam3/bin/python` below.
- weights: the old prototype's `models/sam31-mlx` folder (an MLX conversion
  of SAM 3.1 with `config.json`, `*.safetensors` and a `tokenizer/`
  subfolder). Linking it into this repository keeps paths short and the
  prototype untouched:

```sh
ln -s "<old Auto camera folder>/models/sam31-mlx" ~/repos/auto-camera/models/sam31-mlx
```

Check the venv has the pinned versions (pip shows the git revision of a git
install):

```sh
cd ~/repos/auto-camera
models/sam3/.venv/bin/python -c "import mlx.core as mx, mlx_vlm.version as v, transformers as t; print(mx.__version__, v.__version__, t.__version__)"
models/sam3/.venv/bin/pip freeze | grep -i -e mlx -e transformers
```

The host spawns (paths relative to `~/repos/auto-camera`; a studio config
writes them relative to itself, as `config/sam3-sidecar.example.toml` does
with `[detector] kind = "sidecar"`):

```sh
models/sam3/.venv/bin/python sidecars/sam3-mlx/server.py --backend mlx --model models/sam31-mlx --side 672
```

To try it by hand, close stdin straight away: the sidecar loads the model,
writes one binary `hello` frame to stdout, logs to stderr and exits.

```sh
models/sam3/.venv/bin/python sidecars/sam3-mlx/server.py --model models/sam31-mlx < /dev/null > /tmp/hello.bin
```

The real-model smoke test sends a grey 1280x720 frame, and optionally a
picture with a person in it, through the real server. The picture is
turned upright by its EXIF orientation and shrunk to at most 1008 px on its
longer side, but never with its shorter side below the 672 px input side,
as the Rust host does, so a full-size phone photo works:

```sh
cd ~/repos/auto-camera/sidecars/sam3-mlx
AC_SAM3_MODEL=~/repos/auto-camera/models/sam31-mlx AC_SAM3_IMAGE=~/Pictures/someone.jpg \
  ../../models/sam3/.venv/bin/python -m unittest tests.test_mlx_smoke -v
```

For a fresh venv elsewhere: `python3 -m venv .venv && .venv/bin/pip install -r requirements.txt`.

## Fake backend and tests

`--backend fake` needs only Python 3.10+ and the standard library, on any OS.
It loads nothing and answers deterministically: the k-th distinct phrase of a
request (0-based, first-seen order) gets one box
`[0.05 + 0.1 * (k % 9), 0.2, 0.08, 0.6]` (x, y, w, h) with score 0.9, and the
phrase `fake:error` makes that request fail with an error reply.

```sh
cd sidecars/sam3-mlx
python3 server.py --backend fake < /dev/null | od -c | head   # hello, then exit 0
python3 -m unittest -v                                        # codec, loop, fake, helpers
```

The tests need no third-party package; the MLX glue tests skip themselves
without numpy and the smoke test without `AC_SAM3_MODEL`. Lint with
`ruff check . && ruff format --check .` (settings in `ruff.toml`); CI runs
both with Ruff 0.16.8. The licences of the packages in `requirements.txt`
are in [`docs/licence-manifest.md`](../../docs/licence-manifest.md), and a
new requirement needs an entry there.

## Command line

| Argument | Meaning |
|---|---|
| `--backend mlx\|fake` | detector (default `mlx`) |
| `--model PATH` | MLX weights folder; required for `mlx`, ignored by `fake` |
| `--side N` | square side the model runs at, 224 to 1008 and a multiple of 28 for SAM 3.1 (default 672); checked on the command line, so a bad side exits with 2 at once, before any loading |
| `--log-level debug\|info\|warning\|error` | stderr log level (default `info`) |

## Protocol v1

The authoritative description is the docstring of
[`sam3_sidecar/protocol.py`](sam3_sidecar/protocol.py). In short:

- **Frame:** `u32 LE header_len | UTF-8 JSON header | u32 LE payload_len | payload`;
  header at most 1 MiB, payload at most 64 MiB, anything larger is a protocol
  error. stdout carries frames only; logs go to stderr.
- **hello** (child to host, once the model is loaded and warmed up):
  `{"type":"hello","protocol":1,"model":"sam3.1-mlx","weights_sha256":"<hex>","input_side":672,"capabilities":{"masks":false,"parts":false}}`.
  The fake reports `"model":"fake"` and `"weights_sha256":null`.
- **detect** (host to child, payload `width*height*3` RGB8 bytes, row-major,
  top row first):
  `{"type":"detect","id":1,"source":"cam-1","seq":7,"pts_ns":0,"width":1280,"height":720,"format":"rgb8","phrases":[{"text":"a person speaking","threshold":0.3}],"want_masks":false}`.
  `want_masks` may be omitted (false); true is refused. Unknown fields are
  ignored. An empty `phrases` list gets an empty result.
- **result:** the request's `id`, `source`, `seq` and `pts_ns`, `latency_ms`
  (backend time on a monotonic clock, pipe transfer excluded) and
  `detections: [{"phrase": <index>, "box": [x, y, w, h], "score": s}]`,
  boxes normalized to the image sent, top-left origin, ordered by phrase
  index then falling score, only scores at or above the phrase's threshold.
  A phrase text repeated with different thresholds is run once and answered
  per index.
- **error:** `{"type":"error","id":<id or null>,"message":"..."}`. An invalid
  request or a failure inside the model is answered with its id and the
  sidecar carries on. `id: null` is fatal and the sidecar exits right after.

Exit codes: `0` stdin closed (the shutdown signal), `1` Python crash
(traceback on stderr), `2` bad command line, `3` model failed to load, `4`
the host broke the protocol, `5` the host is gone: stdout closed, or the
parent process ended (a watchdog thread checks `os.getppid()` every 0.25 s
from the start, loading included, so a host that is killed or crashes
leaves no sidecar behind; macOS has no parent-death signal). Cases 2 to 4
send a fatal error frame first.

## What the MLX backend does

- Checks the folder (`config.json`, `tokenizer/`, safetensors) before
  importing MLX, so a wrong path is a clear fatal error in milliseconds.
- Fingerprints the weights for `hello`: the SHA-256 of the single safetensors
  file (`shasum -a 256 model.safetensors`), or for several files the SHA-256
  of their `shasum -a 256` manifest in name order.
- Loads once with local files only (`HF_HUB_OFFLINE=1`), casts to bfloat16,
  points the CLIP tokenizer at `<model>/tokenizer`, sets the processor to the
  `--side` square, and runs one warm-up pass on a grey frame before `hello`.
- Per request: squashes the frame to the square (no letterbox, so normalized
  boxes map straight back to the frame), runs the backbone once and
  mlx-vlm's per-phrase detector on the distinct phrases at the lowest
  threshold asked, then applies each phrase's own threshold.
- Boxes come from each detection's mask with the prototype's 3% speckle rule
  (connected components under 3% of the largest are ignored); an empty mask
  falls back to the detector's box.
- Text embeddings are cached only for phrases in some source's latest request
  (up to 32 sources), so retired prompts are freed.

## Status

Tested here (Linux, no MLX): the codec, request validation, the loop and its
error paths, the server process (stdio routing, exit codes, `python -I`), the
fake backend, the helpers and the weights checks, and the MLX backend's glue
with mlx-vlm stubbed out. **Not run yet:** anything that imports MLX, that is
the real load, inference, OpenCV mask bounds, the version pins and the smoke
test. Run `tests.test_mlx_smoke` on the Mac before relying on it. The
design's bundled-image self-check in `hello` is not built.
