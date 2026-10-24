"""Pure backend helpers, the fake backend and the weights-folder checks.

Normal behaviour, invalid input and edges for each helper; nothing here
needs MLX, numpy or a model.
"""

from __future__ import annotations

import hashlib
import json
import math
import tempfile
import unittest
from collections.abc import Mapping, Sequence
from pathlib import Path
from types import SimpleNamespace

from sam3_sidecar.backend import (
    MAX_SIDE,
    MIN_SIDE,
    Backend,
    Candidate,
    PhraseUse,
    check_side,
    clip_box,
    fan_out,
    prune_cache,
    speckle_bounds,
    unique_texts,
)
from sam3_sidecar.fake import FAIL_PHRASE, FAKE_SCORE, FakeBackend
from sam3_sidecar.mlx_sam31 import configure_resolution
from sam3_sidecar.model_files import (
    ModelFolderError,
    check_model_folder,
    weight_files,
    weights_sha256,
)
from sam3_sidecar.protocol import Detection, DetectRequest, Phrase


def request(*phrases: tuple[str, float], source: str = "cam-1") -> DetectRequest:
    """Build a 2x2 grey request for ``phrases`` given as (text, threshold)."""
    return DetectRequest(
        id=1,
        source=source,
        seq=0,
        pts_ns=0,
        width=2,
        height=2,
        phrases=tuple(Phrase(text, threshold) for text, threshold in phrases),
        pixels=bytes(12),
    )


class RecordingBackend(Backend):
    """A backend that records what ``detect`` asks of ``find``."""

    def __init__(self, found: Mapping[str, Sequence[Candidate]]) -> None:
        """Answer every call with ``found``."""
        super().__init__(model="recording", weights_sha256=None, input_side=672)
        #: The canned answer.
        self.found = found
        #: Each call's (texts, floor).
        self.calls: list[tuple[list[str], float]] = []

    def find(
        self, request: DetectRequest, texts: Sequence[str], floor: float
    ) -> Mapping[str, Sequence[Candidate]]:
        """Record the call and return the canned answer."""
        self.calls.append((list(texts), floor))
        return self.found


class UniqueTextsTests(unittest.TestCase):
    """Collapsing phrases to distinct texts."""

    def test_first_seen_order_with_lowest_threshold(self) -> None:
        """Duplicates keep their first position and the lowest threshold."""
        phrases = [Phrase("b", 0.5), Phrase("a", 0.4), Phrase("b", 0.3), Phrase("b", 0.6)]
        self.assertEqual(list(unique_texts(phrases).items()), [("b", 0.3), ("a", 0.4)])

    def test_texts_are_compared_exactly(self) -> None:
        """Case and spacing differences are different texts."""
        self.assertEqual(len(unique_texts([Phrase("Person", 0.5), Phrase("person", 0.5)])), 2)

    def test_empty(self) -> None:
        """No phrases, no texts."""
        self.assertEqual(unique_texts([]), {})


class ClipBoxTests(unittest.TestCase):
    """Corner boxes to clipped xywh boxes."""

    def test_inside_box_converts(self) -> None:
        """A box inside the image becomes x, y, w, h."""
        x, y, w, h = clip_box((0.1, 0.2, 0.5, 0.9)) or (0, 0, 0, 0)
        self.assertEqual((x, y), (0.1, 0.2))
        self.assertAlmostEqual(w, 0.4)
        self.assertAlmostEqual(h, 0.7)

    def test_outside_parts_are_clipped(self) -> None:
        """Parts beyond the image edges are cut off."""
        self.assertEqual(clip_box((-0.5, -0.1, 1.5, 1.2)), (0.0, 0.0, 1.0, 1.0))

    def test_boxes_without_area_or_finite_corners_are_dropped(self) -> None:
        """Empty, inverted, off-image and non-finite boxes give None."""
        for box in [
            (0.5, 0.5, 0.5, 0.9),
            (0.6, 0.1, 0.4, 0.9),
            (1.2, 0.1, 1.5, 0.9),
            (0.1, math.nan, 0.5, 0.9),
            (0.1, 0.1, math.inf, 0.9),
        ]:
            with self.subTest(box=box):
                self.assertIsNone(clip_box(box))


class FanOutTests(unittest.TestCase):
    """Giving each phrase index its own detections."""

    def test_each_index_gets_matches_at_or_above_its_threshold(self) -> None:
        """Duplicated texts are answered once per index, by their thresholds."""
        found = {
            "person": [Candidate((0.1, 0.1, 0.2, 0.2), 0.4), Candidate((0.5, 0.5, 0.6, 0.6), 0.8)],
            "drum": [Candidate((0.0, 0.0, 1.0, 1.0), 0.6)],
        }
        phrases = [Phrase("person", 0.8), Phrase("drum", 0.7), Phrase("person", 0.3)]
        got = [(d.phrase, d.score) for d in fan_out(phrases, found)]
        self.assertEqual(got, [(0, 0.8), (2, 0.8), (2, 0.4)])

    def test_missing_texts_and_bad_candidates_are_skipped(self) -> None:
        """Invalid scores and boxes never become detections."""
        found = {
            "a": [
                Candidate((0.1, 0.1, 0.2, 0.2), math.nan),
                Candidate((0.1, 0.1, 0.2, 0.2), 1.5),
                Candidate((0.1, 0.1, 0.2, 0.2), -0.1),
                Candidate((0.3, 0.3, 0.3, 0.4), 0.9),
                Candidate((0.1, 0.1, 0.2, 0.2), 0.7),
            ]
        }
        detections = fan_out([Phrase("a", 0.0), Phrase("absent", 0.0)], found)
        self.assertEqual(len(detections), 1)
        self.assertIsInstance(detections[0], Detection)
        self.assertEqual(detections[0].score, 0.7)

    def test_detect_runs_find_once_with_the_lowest_threshold(self) -> None:
        """``Backend.detect`` dedupes texts and asks for the lowest floor."""
        backend = RecordingBackend({"a": [Candidate((0.1, 0.1, 0.2, 0.2), 0.5)]})
        out = backend.detect(request(("a", 0.6), ("b", 0.2), ("a", 0.4)))
        self.assertEqual(backend.calls, [(["a", "b"], 0.2)])
        self.assertEqual([d.phrase for d in out], [2])

    def test_detect_skips_the_model_without_phrases(self) -> None:
        """An empty phrase list never reaches ``find``."""
        backend = RecordingBackend({})
        self.assertEqual(backend.detect(request()), [])
        self.assertEqual(backend.calls, [])


class SpeckleBoundsTests(unittest.TestCase):
    """The PoC's 3% speckle rule on mask components."""

    def test_empty_mask_has_no_bounds(self) -> None:
        """No foreground components, no box."""
        self.assertIsNone(speckle_bounds([], 10, 10))
        self.assertIsNone(speckle_bounds([(0, 0, 1, 1, 0)], 10, 10))

    def test_single_component_uses_exclusive_edges(self) -> None:
        """A component's box spans left..left+width, normalized."""
        self.assertEqual(speckle_bounds([(2, 1, 4, 3, 12)], 10, 5), (0.2, 0.2, 0.6, 0.8))

    def test_specks_below_three_percent_are_ignored(self) -> None:
        """A speck under 3% of the largest component does not stretch the box."""
        body = (10, 10, 20, 50, 1000)
        speck = (90, 90, 1, 1, 29)
        self.assertEqual(speckle_bounds([body, speck], 100, 100), (0.1, 0.1, 0.3, 0.6))

    def test_parts_at_three_percent_or_more_are_kept(self) -> None:
        """A detached part of exactly 3% (a hand, a guitar neck) is kept."""
        body = (10, 10, 20, 50, 1000)
        hand = (80, 5, 5, 6, 30)
        self.assertEqual(speckle_bounds([body, hand], 100, 100), (0.1, 0.05, 0.85, 0.6))

    def test_invalid_arguments_raise(self) -> None:
        """Non-positive sizes and relative areas outside (0, 1] are errors."""
        with self.assertRaises(ValueError):
            speckle_bounds([], 0, 10)
        for area in (0.0, -0.1, 1.5):
            with self.subTest(area=area), self.assertRaises(ValueError):
                speckle_bounds([], 10, 10, minimum_relative_area=area)


class ResolutionTests(unittest.TestCase):
    """The ported side check and processor configuration."""

    def test_valid_sides(self) -> None:
        """672 and both bounds are multiples of 28 in range."""
        for side in (MIN_SIDE, 672, MAX_SIDE):
            with self.subTest(side=side):
                check_side(side, 14)

    def test_invalid_sides(self) -> None:
        """Out-of-range sides and odd patch grids are refused."""
        for side in (MIN_SIDE - 28, MAX_SIDE + 28, 686, 0):
            with self.subTest(side=side), self.assertRaises(ValueError):
                check_side(side, 14)
        with self.assertRaises(ValueError):
            check_side(672, 0)

    def test_configure_resolution_sets_the_processor(self) -> None:
        """The processor squashes to the side; the patch grid is returned."""
        model = SimpleNamespace(
            detector_model=SimpleNamespace(
                vision_encoder=SimpleNamespace(
                    backbone=SimpleNamespace(config=SimpleNamespace(patch_size=14))
                )
            )
        )
        processor = SimpleNamespace(image_size=1008)
        self.assertEqual(configure_resolution(model, processor, 672), 48)
        self.assertEqual(processor.image_size, 672)
        with self.assertRaises(ValueError):
            configure_resolution(model, processor, 686)
        self.assertEqual(processor.image_size, 672)


class PhraseUseTests(unittest.TestCase):
    """Bounding the text-embedding cache by current use."""

    def test_union_of_each_sources_latest_texts(self) -> None:
        """A source's new request replaces its old texts, others stay."""
        use = PhraseUse()
        use.record("cam-1", ["a", "b"])
        use.record("cam-2", ["c"])
        use.record("cam-1", ["b"])
        self.assertEqual(use.in_use(), {"b", "c"})

    def test_least_recently_heard_source_is_forgotten(self) -> None:
        """Past the limit, the source heard from longest ago drops out."""
        use = PhraseUse(limit=2)
        use.record("cam-1", ["a"])
        use.record("cam-2", ["b"])
        use.record("cam-1", ["a"])
        use.record("cam-3", ["c"])
        self.assertEqual(use.in_use(), {"a", "c"})

    def test_limit_must_be_positive(self) -> None:
        """A zero limit is refused."""
        with self.assertRaises(ValueError):
            PhraseUse(limit=0)

    def test_prune_cache_drops_only_unused_keys(self) -> None:
        """Entries for texts in use survive; the count of removals is returned."""
        cache: dict[str, object] = {"a": 1, "b": 2, "c": 3}
        self.assertEqual(prune_cache(cache, {"b", "z"}), 2)
        self.assertEqual(cache, {"b": 2})
        self.assertEqual(prune_cache(cache, {"b"}), 0)


class FakeBackendTests(unittest.TestCase):
    """The deterministic fake."""

    def test_kth_text_gets_the_documented_box(self) -> None:
        """Boxes follow 0.05 + 0.1 * (k % 9), wrapping after nine texts."""
        texts = [(f"t{k}", 0.5) for k in range(10)]
        detections = FakeBackend(672).detect(request(*texts))
        self.assertEqual([d.phrase for d in detections], list(range(10)))
        for k, detection in enumerate(detections):
            x, y, w, h = detection.box
            self.assertAlmostEqual(x, 0.05 + 0.1 * (k % 9))
            self.assertAlmostEqual(y, 0.2)
            self.assertAlmostEqual(w, 0.08)
            self.assertAlmostEqual(h, 0.6)
            self.assertEqual(detection.score, FAKE_SCORE)

    def test_same_request_same_answer(self) -> None:
        """The fake is deterministic."""
        backend = FakeBackend(672)
        req = request(("a", 0.5), ("b", 0.5))
        self.assertEqual(backend.detect(req), backend.detect(req))

    def test_threshold_above_the_fake_score_finds_nothing(self) -> None:
        """Every fake score is 0.9."""
        self.assertEqual(FakeBackend(672).detect(request(("a", 0.95))), [])

    def test_fail_phrase_raises(self) -> None:
        """The failure phrase makes the request raise."""
        with self.assertRaises(RuntimeError):
            FakeBackend(672).detect(request(("a", 0.5), (FAIL_PHRASE, 0.5)))

    def test_hello_facts(self) -> None:
        """The fake names itself and has no weights."""
        backend = FakeBackend(448)
        self.assertEqual(
            (backend.model, backend.weights_sha256, backend.input_side), ("fake", None, 448)
        )


class ModelFilesTests(unittest.TestCase):
    """Weights-folder checks and fingerprints, on temporary folders."""

    def setUp(self) -> None:
        """Make an empty temporary folder for each test."""
        self._tmp = tempfile.TemporaryDirectory()
        self.folder = Path(self._tmp.name)

    def tearDown(self) -> None:
        """Remove the temporary folder."""
        self._tmp.cleanup()

    def write(self, name: str, data: bytes) -> Path:
        """Create ``name`` in the folder with ``data``."""
        path = self.folder / name
        path.write_bytes(data)
        return path

    def make_complete(self) -> None:
        """Lay out a minimal valid weights folder."""
        self.write("config.json", b"{}")
        (self.folder / "tokenizer").mkdir()
        self.write("model.safetensors", b"weights")

    def test_complete_folder_passes(self) -> None:
        """config.json, tokenizer/ and a safetensors file are enough."""
        self.make_complete()
        check_model_folder(self.folder)

    def test_each_missing_piece_is_named(self) -> None:
        """The error says what is missing."""
        with self.assertRaisesRegex(ModelFolderError, "does not exist"):
            check_model_folder(self.folder / "absent")
        with self.assertRaisesRegex(ModelFolderError, "config.json"):
            check_model_folder(self.folder)
        self.write("config.json", b"{}")
        with self.assertRaisesRegex(ModelFolderError, "tokenizer"):
            check_model_folder(self.folder)
        (self.folder / "tokenizer").mkdir()
        with self.assertRaisesRegex(ModelFolderError, "safetensors"):
            check_model_folder(self.folder)
        with self.assertRaisesRegex(ModelFolderError, "does not exist"):
            check_model_folder(self.folder / "config.json")

    def test_weight_files_follow_load_model(self) -> None:
        """Glob order by name, ignoring consolidated.safetensors."""
        self.write("b.safetensors", b"2")
        self.write("a.safetensors", b"1")
        self.write("consolidated.safetensors", b"x")
        self.assertEqual(
            [p.name for p in weight_files(self.folder)], ["a.safetensors", "b.safetensors"]
        )

    def test_weight_files_prefer_existing_index_shards(self) -> None:
        """An index names the shards; missing shards fall back to the glob."""
        self.write("a.safetensors", b"1")
        self.write("b.safetensors", b"2")
        index = {"weight_map": {"w1": "b.safetensors", "w2": "b.safetensors"}}
        self.write("model.safetensors.index.json", json.dumps(index).encode())
        self.assertEqual([p.name for p in weight_files(self.folder)], ["b.safetensors"])
        self.write(
            "model.safetensors.index.json",
            json.dumps({"weight_map": {"w": "gone.safetensors"}}).encode(),
        )
        self.assertEqual(len(weight_files(self.folder)), 2)
        self.write("model.safetensors.index.json", b"not json")
        self.assertEqual(len(weight_files(self.folder)), 2)

    def test_single_file_digest_is_its_sha256(self) -> None:
        """One weights file: the plain SHA-256 of its bytes."""
        self.write("model.safetensors", b"weights")
        self.assertEqual(weights_sha256(self.folder), hashlib.sha256(b"weights").hexdigest())

    def test_several_files_digest_their_manifest(self) -> None:
        """Several files: SHA-256 of the shasum-style manifest in name order."""
        self.write("b.safetensors", b"2")
        self.write("a.safetensors", b"1")
        manifest = (
            f"{hashlib.sha256(b'1').hexdigest()}  a.safetensors\n"
            f"{hashlib.sha256(b'2').hexdigest()}  b.safetensors\n"
        )
        self.assertEqual(weights_sha256(self.folder), hashlib.sha256(manifest.encode()).hexdigest())

    def test_digest_without_weights_fails(self) -> None:
        """No weights, no fingerprint."""
        with self.assertRaises(ModelFolderError):
            weights_sha256(self.folder)


if __name__ == "__main__":
    unittest.main()
