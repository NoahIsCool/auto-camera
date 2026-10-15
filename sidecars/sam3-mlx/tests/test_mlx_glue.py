"""The MLX backend's glue around mlx-vlm, with every MLX call stubbed.

The stubs return what the pinned mlx-vlm functions return (numpy boxes in
pixels of the S-square, ``(N, S, S)`` uint8 masks, per-row prompt labels), so
these tests check our side of the contract: pixel decoding, the threshold
margin, box normalization, mask bounds with the speckle rule, grouping by
label and text-cache pruning. Whether mlx-vlm really behaves like the stubs
is only checked on a Mac (``test_mlx_smoke``).

Skipped when numpy is not installed; no MLX or OpenCV is needed.
"""

from __future__ import annotations

import unittest
from collections import deque
from types import SimpleNamespace
from typing import Any

from sam3_sidecar.mlx_sam31 import Sam31Backend
from sam3_sidecar.protocol import DetectRequest, Phrase

try:
    import numpy as np
except ImportError:  # pragma: no cover - depends on the machine
    np = None

#: Side of the stub model's square; 40 px keeps masks small but speckles rare.
SIDE = 40


def label_components(binary: Any) -> tuple[int, Any, Any, Any]:
    """Stand in for ``cv2.connectedComponentsWithStats`` (8-connectivity).

    Returns ``(count, labels, stats, centroids)`` with background as label 0
    and ``stats`` rows of left, top, width, height and area.
    """
    height, width = binary.shape
    labels = np.zeros((height, width), dtype=np.int32)
    stats = [[0, 0, width, height, int((binary == 0).sum())]]
    for y in range(height):
        for x in range(width):
            if not binary[y, x] or labels[y, x]:
                continue
            label = len(stats)
            labels[y, x] = label
            queue, pixels = deque([(y, x)]), []
            while queue:
                cy, cx = queue.popleft()
                pixels.append((cy, cx))
                for ny in range(max(cy - 1, 0), min(cy + 2, height)):
                    for nx in range(max(cx - 1, 0), min(cx + 2, width)):
                        if binary[ny, nx] and not labels[ny, nx]:
                            labels[ny, nx] = label
                            queue.append((ny, nx))
            ys = [p[0] for p in pixels]
            xs = [p[1] for p in pixels]
            box = [min(xs), min(ys), max(xs) - min(xs) + 1, max(ys) - min(ys) + 1, len(pixels)]
            stats.append(box)
    return len(stats), labels, np.array(stats, dtype=np.int32), None


class StubArray:
    """What ``mx.array`` returns: remembers the values and the cast."""

    def __init__(self, values: Any) -> None:
        """Wrap ``values``."""
        #: The wrapped numpy values.
        self.values = values
        #: The dtype passed to ``astype``, if any.
        self.dtype: Any = None

    def astype(self, dtype: Any) -> StubArray:
        """Record the cast and return self."""
        self.dtype = dtype
        return self


@unittest.skipIf(np is None, "numpy is not installed")
class Sam31GlueTests(unittest.TestCase):
    """``Sam31Backend.find`` and ``warm_up`` against stubbed mlx-vlm."""

    def build(self, result: Any) -> tuple[Sam31Backend, dict[str, Any]]:
        """Build a backend whose mlx-vlm calls return ``result`` and are recorded."""
        calls: dict[str, Any] = {}

        def backbone(model: Any, pixels: StubArray) -> str:
            calls["backbone"] = pixels
            return "features"

        def detect(
            predictor: Any, features: Any, prompts: list[str], size: Any, threshold: float
        ) -> Any:
            calls["detect"] = (features, list(prompts), size, threshold)
            return result

        def preprocess_image(rgb: Any) -> dict[str, Any]:
            calls["rgb"] = np.array(rgb)
            return {"pixel_values": np.zeros((1, SIDE, SIDE, 3), dtype=np.float32)}

        modules = {
            "np": np,
            "cv2": SimpleNamespace(
                connectedComponentsWithStats=lambda b, connectivity: label_components(b)
            ),
            "mx": SimpleNamespace(array=StubArray, bfloat16="bf16", get_peak_memory=lambda: 0),
            "backbone": backbone,
            "detect": detect,
        }
        predictor = SimpleNamespace(_text_cache={"retired prompt": 1, "person": 2})
        backend = Sam31Backend(
            modules=modules,
            model="model",
            processor=SimpleNamespace(preprocess_image=preprocess_image),
            predictor=predictor,
            input_side=SIDE,
            digest="d" * 64,
        )
        return backend, calls

    def request(self, *phrases: tuple[str, float]) -> DetectRequest:
        """Build a 3x2 request whose pixels count up, so decoding order shows."""
        return DetectRequest(
            id=1,
            source="cam-1",
            seq=2,
            pts_ns=3,
            width=3,
            height=2,
            phrases=tuple(Phrase(t, th) for t, th in phrases),
            pixels=bytes(range(18)),
        )

    def result(
        self, boxes: list[list[float]], masks: Any, scores: list[float], labels: list[str]
    ) -> Any:
        """Build a ``DetectionResult``-shaped answer."""
        return SimpleNamespace(
            boxes=np.array(boxes, dtype=np.float32).reshape(-1, 4),
            masks=masks,
            scores=np.array(scores, dtype=np.float32),
            labels=labels,
        )

    def test_one_backbone_then_all_texts_with_a_lowered_threshold(self) -> None:
        """Pixels decode row-major; detect gets the texts, the square and floor - margin."""
        backend, calls = self.build(self.result([], np.zeros((0, SIDE, SIDE), np.uint8), [], []))
        backend.detect(self.request(("person", 0.5), ("drummer", 0.3), ("person", 0.4)))
        self.assertEqual(calls["rgb"].shape, (2, 3, 3))
        self.assertEqual(calls["rgb"][1, 0].tolist(), [9, 10, 11])
        self.assertEqual(calls["backbone"].dtype, "bf16")
        features, prompts, size, threshold = calls["detect"]
        self.assertEqual(
            (features, prompts, size), ("features", ["person", "drummer"], (SIDE, SIDE))
        )
        self.assertLess(threshold, 0.3)
        self.assertAlmostEqual(threshold, 0.3, places=5)

    def test_mask_bounds_replace_the_box_and_ignore_speckles(self) -> None:
        """A nonempty mask gives the box; a 1 px speck far away is ignored."""
        mask = np.zeros((1, SIDE, SIDE), dtype=np.uint8)
        mask[0, 8:28, 4:24] = 1
        mask[0, 38, 38] = 1
        result = self.result([[0, 0, 40, 40]], mask, [0.8], ["person"])
        backend, _ = self.build(result)
        (detection,) = backend.detect(self.request(("person", 0.5)))
        for got, want in zip(detection.box, (0.1, 0.2, 0.5, 0.5), strict=True):
            self.assertAlmostEqual(got, want)
        self.assertAlmostEqual(detection.score, 0.8, places=6)

    def test_empty_mask_falls_back_to_the_model_box(self) -> None:
        """No mask foreground: the detector's pixel box, normalized by the side."""
        result = self.result(
            [[4, 8, 20, 32]], np.zeros((1, SIDE, SIDE), np.uint8), [0.6], ["person"]
        )
        backend, _ = self.build(result)
        (detection,) = backend.detect(self.request(("person", 0.5)))
        for got, want in zip(detection.box, (0.1, 0.2, 0.4, 0.6), strict=True):
            self.assertAlmostEqual(got, want)

    def test_rows_are_grouped_by_label_and_thresholded_per_phrase(self) -> None:
        """Each row joins its prompt; each phrase applies its own threshold."""
        masks = np.zeros((3, SIDE, SIDE), np.uint8)
        result = self.result(
            [[0, 0, 10, 10], [10, 10, 20, 20], [20, 20, 30, 30]],
            masks,
            [0.9, 0.35, 0.45],
            ["a", "b", "a"],
        )
        backend, _ = self.build(result)
        detections = backend.detect(self.request(("a", 0.5), ("b", 0.3), ("a", 0.4)))
        self.assertEqual([d.phrase for d in detections], [0, 1, 2, 2])

    def test_text_cache_keeps_only_texts_in_use(self) -> None:
        """Embeddings of prompts no source asks for any more are dropped."""
        backend, _ = self.build(self.result([], np.zeros((0, SIDE, SIDE), np.uint8), [], []))
        backend.detect(self.request(("person", 0.5)))
        self.assertEqual(set(backend._predictor._text_cache), {"person"})

    def test_warm_up_runs_a_grey_frame_and_leaves_no_embedding(self) -> None:
        """The warm-up pass uses a grey square and prunes its own prompt."""
        backend, calls = self.build(self.result([], np.zeros((0, SIDE, SIDE), np.uint8), [], []))
        backend.warm_up()
        self.assertEqual(calls["rgb"].shape, (SIDE, SIDE, 3))
        self.assertTrue((calls["rgb"] == 128).all())
        self.assertEqual(calls["detect"][1], ["person"])
        self.assertEqual(backend._predictor._text_cache, {})


if __name__ == "__main__":
    unittest.main()
