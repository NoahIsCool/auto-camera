r"""Real SAM 3.1 through the real server, on a Mac with the weights.

Skipped unless ``AC_SAM3_MODEL`` names the MLX weights folder; run it with
the venv's Python so MLX is importable::

    AC_SAM3_MODEL=~/repos/auto-camera/models/sam31-mlx \\
    AC_SAM3_IMAGE=~/Pictures/someone.jpg \\
    ~/repos/auto-camera/models/sam3/.venv/bin/python -m unittest tests.test_mlx_smoke -v

``AC_SAM3_IMAGE`` is optional: a picture with at least one person in it,
decoded with Pillow (from the venv). Without it only a grey frame is sent.
"""

from __future__ import annotations

import os
import sys
import unittest
from pathlib import Path

from sam3_sidecar.loop import EXIT_OK

from .support import detect_bytes, detect_header, run_server

#: Weights folder for the smoke test, if configured.
MODEL = os.environ.get("AC_SAM3_MODEL")

#: Optional picture with at least one person.
IMAGE = os.environ.get("AC_SAM3_IMAGE")

#: Load, fingerprint and warm-up allowance; the first run compiles kernels.
TIMEOUT_S = 300.0


def _image_request(path: str, request_id: int) -> bytes:
    """Encode ``path`` as an RGB8 ``detect`` asking for ``a person``."""
    from PIL import Image

    with Image.open(Path(path).expanduser()) as picture:
        rgb = picture.convert("RGB")
        header = detect_header(
            request_id,
            width=rgb.width,
            height=rgb.height,
            phrases=[{"text": "a person", "threshold": 0.5}],
        )
        return detect_bytes(header, rgb.tobytes())


@unittest.skipUnless(MODEL, "set AC_SAM3_MODEL to the MLX SAM 3.1 weights folder")
class RealModelSmokeTests(unittest.TestCase):
    """hello, a grey frame and optionally a real picture, then a clean exit."""

    def test_real_model_answers_over_the_protocol(self) -> None:
        """The model loads, answers each request with its id and exits on EOF."""
        assert MODEL is not None
        stdin = detect_bytes(detect_header(1, width=1280, height=720))
        if IMAGE:
            stdin += _image_request(IMAGE, 2)
        args = ["--backend", "mlx", "--model", str(Path(MODEL).expanduser()), "--side", "672"]
        code, frames, stderr = run_server(args, stdin, timeout=TIMEOUT_S)
        sys.stderr.write(stderr)
        self.assertEqual(code, EXIT_OK, stderr)
        hello = frames[0].header
        self.assertEqual(hello["type"], "hello")
        self.assertEqual((hello["model"], hello["input_side"]), ("sam3.1-mlx", 672))
        self.assertRegex(hello["weights_sha256"], "^[0-9a-f]{64}$")
        results = [f.header for f in frames[1:]]
        self.assertEqual([r["type"] for r in results], ["result"] * len(results))
        self.assertEqual([r["id"] for r in results], [1, 2] if IMAGE else [1])
        for result in results:
            sys.stderr.write(f"request {result['id']}: {result['latency_ms']:.0f} ms, ")
            sys.stderr.write(f"{result['detections']}\n")
            for detection in result["detections"]:
                x, y, w, h = detection["box"]
                self.assertTrue(0 <= x and 0 <= y and w > 0 and h > 0)
                self.assertTrue(x + w <= 1 + 1e-9 and y + h <= 1 + 1e-9)
                self.assertGreaterEqual(detection["score"], 0.5)
        if IMAGE:
            self.assertGreaterEqual(len(results[1]["detections"]), 1)


if __name__ == "__main__":
    unittest.main()
