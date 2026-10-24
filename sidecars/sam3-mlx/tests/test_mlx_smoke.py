r"""Real SAM 3.1 through the real server, on a Mac with the weights.

Skipped unless ``AC_SAM3_MODEL`` names the MLX weights folder; run it with
the venv's Python so MLX is importable::

    AC_SAM3_MODEL=~/repos/auto-camera/models/sam31-mlx \\
    AC_SAM3_IMAGE=~/Pictures/someone.jpg \\
    ~/repos/auto-camera/models/sam3/.venv/bin/python -m unittest tests.test_mlx_smoke -v

``AC_SAM3_IMAGE`` is optional: a picture with at least one person in it,
decoded with Pillow (from the venv). Without it only a grey frame is sent.
The picture is sent as the Rust host sends a frame: turned upright by its
EXIF orientation and shrunk so its longer side is at most 1008 px (the
host's ``DEFAULT_SEND_SIDE``), so a 24 MP phone photo fits the protocol's
64 MiB payload limit and a portrait shot is not sent sideways.
"""

from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any

from sam3_sidecar.loop import EXIT_OK

from .support import detect_bytes, detect_header, run_server

#: Weights folder for the smoke test, if configured.
MODEL = os.environ.get("AC_SAM3_MODEL")

#: Optional picture with at least one person.
IMAGE = os.environ.get("AC_SAM3_IMAGE")

#: Load, fingerprint and warm-up allowance; the first run compiles kernels.
TIMEOUT_S = 300.0

#: Longest side of a picture sent, as the Rust host's ``DEFAULT_SEND_SIDE``.
SEND_SIDE = 1008

try:
    import PIL  # noqa: F401

    HAVE_PIL = True
except ImportError:
    HAVE_PIL = False


def prepare_picture(path: str | Path) -> Any:
    """Open ``path`` as the host would send it: upright, RGB, longer side <= 1008."""
    from PIL import Image, ImageOps

    with Image.open(Path(path).expanduser()) as picture:
        upright = ImageOps.exif_transpose(picture)
        rgb = upright.convert("RGB")
    rgb.thumbnail((SEND_SIDE, SEND_SIDE))
    return rgb


def _image_request(path: str, request_id: int) -> bytes:
    """Encode ``path`` as an RGB8 ``detect`` asking for ``a person``."""
    rgb = prepare_picture(path)
    header = detect_header(
        request_id,
        width=rgb.width,
        height=rgb.height,
        phrases=[{"text": "a person", "threshold": 0.5}],
    )
    return detect_bytes(header, rgb.tobytes())


@unittest.skipUnless(HAVE_PIL, "needs Pillow")
class PicturePrepTests(unittest.TestCase):
    """The smoke test's picture is sent the way the host sends a frame."""

    def test_large_rotated_pictures_are_turned_upright_and_shrunk(self) -> None:
        """A sideways 24 MP photo comes out upright with its longer side at 1008."""
        from PIL import Image

        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "portrait.jpg"
            stored = Image.new("RGB", (5712, 4284), (200, 120, 40))
            exif = Image.Exif()
            exif[0x0112] = 6  # Orientation: rotate 90 degrees clockwise to view.
            stored.save(path, exif=exif)
            prepared = prepare_picture(path)
        self.assertEqual(prepared.size, (756, 1008))
        self.assertEqual(prepared.mode, "RGB")
        request = _image_request_from(prepared)
        self.assertLess(len(request), 64 * 1024 * 1024)

    def test_small_pictures_are_not_enlarged(self) -> None:
        """A picture already within 1008 px keeps its size."""
        from PIL import Image

        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "small.png"
            Image.new("RGBA", (640, 360)).save(path)
            self.assertEqual(prepare_picture(path).size, (640, 360))


def _image_request_from(rgb: Any) -> bytes:
    """Encode an already prepared picture as a ``detect``."""
    header = detect_header(3, width=rgb.width, height=rgb.height)
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
