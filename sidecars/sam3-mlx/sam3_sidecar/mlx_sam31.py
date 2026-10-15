"""SAM 3.1 image detection through mlx-vlm, for Apple Silicon.

Theory
------
SAM 3.1's detector splits into a ViT backbone that sees only the image and a
DETR encoder, decoder and mask head that see the image features and one text
prompt. On Noah's M3 Max at 672 px the backbone costs about 365 ms and each
prompt adds 77 to 118 ms (measured, ``research/sam31-vs-owlv2.md``), so a
request runs the backbone once and then the prompt side once per distinct
text: 0.48 s for 1 prompt, 0.64 s for 3, 1.05 s for 9.

Every mlx-vlm call below was checked against the pinned revision
5b22c24f325a953ec70b965e39fb97a9b5ec7a4d, fetched from
``raw.githubusercontent.com/Blaizzy/mlx-vlm/<rev>/mlx_vlm/...``:

* ``utils.py``: ``load_model(model_path: Path, lazy=False, strict=True)``
  loads ``config.json`` and the safetensors and evaluates the parameters.
* ``models/sam3_1/processing_sam3_1.py``: ``Sam31Processor`` is
  ``Sam3Processor`` (``models/sam3/processing_sam3.py``), whose
  ``from_pretrained`` reads only ``processor_config.json`` for a local
  folder, whose ``tokenizer`` property uses ``_tokenizer`` when set, and whose
  ``preprocess_image`` squashes to ``image_size`` square (PIL bilinear) and
  normalizes with mean = std = 0.5 into ``(1, S, S, 3)`` float32.
* ``models/sam3/generate.py``: ``Sam3Predictor(model, processor)`` caches
  ``(inputs_embeds, attention_mask)`` per prompt string in ``_text_cache``
  forever, hence ``prune_cache``.
* ``models/sam3_1/generate.py``: ``_get_backbone_features(model, pixels)``
  runs only the ViT; ``_detect_with_backbone(predictor, features, prompts,
  image_size, threshold)`` runs the FPN neck once and then, per prompt, the
  DETR encoder and decoder, scoring (sigmoid(logit) x sigmoid(presence)),
  the mask decoder, ``scores > threshold`` and per-prompt NMS at IoU 0.5. With
  ``image_size=(S, S)`` its boxes are xyxy pixels of the S-square and its
  masks are ``(N, S, S)`` uint8; ``labels`` names each row's prompt.
* ``models/sam3_1/sam3_1.py`` and ``models/sam3_1/vision.py``:
  ``model.detector_model.vision_encoder.backbone`` is the ViT whose
  ``config.patch_size`` sets the grid.
* ``models/sam3/vision.py``: ``ViTBackbone.__call__`` tiles its position
  embeddings and recomputes its global RoPE for any grid divisible by the
  patch size, so the detector needs no other change at 672 px. The PoC's
  ``sam31_resolution.configure_resolution`` also re-grids the tracker's
  memory attention and mask downsampler; image detection never touches the
  tracker, so only the side check and ``processor.image_size`` are ported.

Box choice: when a detection's mask has foreground, its box is the mask's
bounds with speckles removed (``backend.speckle_bounds``, the PoC's 3% rule);
an empty mask falls back to the detector's own box.

Invariants
----------
* MLX, mlx-vlm, transformers, numpy and OpenCV are imported only inside
  ``Sam31Backend.load``, so the protocol and the fake backend need none.
* Weights and tokenizer are read from local files only: Hugging Face's
  offline switches are forced on before any of its code is imported.
* The model is loaded once; each request runs the backbone exactly once
  (none for an empty phrase list) and then one prompt pass per distinct text.
* Text embeddings stay cached only for texts in some source's latest request.
* A warm-up pass runs before ``hello`` so the first real request does not pay
  for kernel compilation.
"""

from __future__ import annotations

import logging
import os
import time
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any

from .backend import Backend, BoxXyxy, Candidate, PhraseUse, check_side, prune_cache, speckle_bounds
from .model_files import check_model_folder, weights_sha256
from .protocol import DetectRequest

#: Model name reported in ``hello``.
MODEL_NAME = "sam3.1-mlx"

#: MLX buffer cache bound, as in the PoC's ``Sam31Detector``.
_CACHE_LIMIT_BYTES = 512 << 20

#: mlx-vlm keeps ``score > threshold``; asking for this much less keeps
#: scores exactly at a phrase threshold, and ``fan_out`` applies ``>=``.
_THRESHOLD_MARGIN = 1e-6

#: Text of the warm-up pass.
_WARM_UP_TEXT = "person"

#: Threshold of the warm-up pass.
_WARM_UP_THRESHOLD = 0.5

#: Grey level of the warm-up image.
_WARM_UP_GREY = 128

#: Logger for this backend; the server routes it to stderr.
_LOG = logging.getLogger("sam3_sidecar.mlx")


class Sam31Backend(Backend):
    """SAM 3.1 through mlx-vlm: one backbone per frame, one pass per text."""

    def __init__(
        self,
        *,
        modules: Mapping[str, Any],
        model: Any,
        processor: Any,
        predictor: Any,
        input_side: int,
        digest: str | None,
    ) -> None:
        """Wrap an already loaded model; use ``load`` to build one."""
        super().__init__(model=MODEL_NAME, weights_sha256=digest, input_side=input_side)
        #: Lazily imported modules and functions, by short name.
        self._m = dict(modules)
        #: The mlx-vlm SAM 3.1 model.
        self._model = model
        #: The ``Sam31Processor`` configured to ``input_side``.
        self._processor = processor
        #: The ``Sam3Predictor`` that owns the text-embedding cache.
        self._predictor = predictor
        #: Which texts each source asked for last; bounds the text cache.
        self._use = PhraseUse()

    @classmethod
    def load(cls, folder: Path, side: int) -> Sam31Backend:
        """Check, fingerprint and load the weights, then warm the model up.

        Raises on any failure; the server turns that into a fatal error.
        """
        check_model_folder(folder)
        started = time.perf_counter()
        digest = weights_sha256(folder)
        _LOG.info("weights sha256 %s (%.1f s)", digest, time.perf_counter() - started)

        # Local files only: these switches are read when huggingface_hub and
        # transformers are imported, so they must be set first.
        os.environ["HF_HUB_OFFLINE"] = "1"
        os.environ["TRANSFORMERS_OFFLINE"] = "1"
        started = time.perf_counter()
        import cv2
        import mlx.core as mx
        import numpy as np
        from mlx_vlm.models.sam3.generate import Sam3Predictor
        from mlx_vlm.models.sam3_1.generate import _detect_with_backbone, _get_backbone_features
        from mlx_vlm.models.sam3_1.processing_sam3_1 import Sam31Processor
        from mlx_vlm.utils import load_model
        from transformers import CLIPTokenizer

        mx.set_cache_limit(_CACHE_LIMIT_BYTES)
        model = load_model(folder, strict=True)
        model.set_dtype(mx.bfloat16)
        mx.eval(model.parameters())
        processor = Sam31Processor.from_pretrained(str(folder))
        processor._tokenizer = CLIPTokenizer.from_pretrained(
            str(folder / "tokenizer"), local_files_only=True
        )
        configure_resolution(model, processor, side)
        predictor = Sam3Predictor(model, processor)
        _LOG.info("model loaded at %d px (%.1f s)", side, time.perf_counter() - started)

        modules = {
            "cv2": cv2,
            "mx": mx,
            "np": np,
            "backbone": _get_backbone_features,
            "detect": _detect_with_backbone,
        }
        backend = cls(
            modules=modules,
            model=model,
            processor=processor,
            predictor=predictor,
            input_side=side,
            digest=digest,
        )
        backend.warm_up()
        return backend

    def warm_up(self) -> None:
        """Run one pass on a grey frame, then drop its text embedding."""
        np = self._m["np"]
        started = time.perf_counter()
        side = self.input_side
        grey = np.full((side, side, 3), _WARM_UP_GREY, dtype=np.uint8)
        self._infer(grey, [_WARM_UP_TEXT], _WARM_UP_THRESHOLD)
        prune_cache(self._predictor._text_cache, self._use.in_use())
        peak = self._m["mx"].get_peak_memory() / (1 << 30)
        _LOG.info("warm-up done (%.2f s, peak %.2f GiB)", time.perf_counter() - started, peak)

    def find(
        self, request: DetectRequest, texts: Sequence[str], floor: float
    ) -> Mapping[str, Sequence[Candidate]]:
        """Prune the text cache, decode the pixels and run the model."""
        self._use.record(request.source, texts)
        dropped = prune_cache(self._predictor._text_cache, self._use.in_use())
        if dropped:
            _LOG.debug("dropped %d unused text embeddings", dropped)
        np = self._m["np"]
        rgb = np.frombuffer(request.pixels, dtype=np.uint8).reshape(
            request.height, request.width, 3
        )
        return self._infer(rgb, texts, floor)

    def _infer(self, rgb: Any, texts: Sequence[str], floor: float) -> dict[str, list[Candidate]]:
        """Backbone once, then mlx-vlm's per-text detection; normalize boxes."""
        mx, np = self._m["mx"], self._m["np"]
        side = self.input_side
        pixels = mx.array(self._processor.preprocess_image(rgb)["pixel_values"])
        features = self._m["backbone"](self._model, pixels.astype(mx.bfloat16))
        result = self._m["detect"](
            self._predictor, features, list(texts), (side, side), floor - _THRESHOLD_MARGIN
        )
        labels = list(result.labels or [])
        boxes = np.asarray(result.boxes, dtype=np.float64).reshape(-1, 4) / side
        scores = np.asarray(result.scores, dtype=np.float64).reshape(-1)
        masks = result.masks
        found: dict[str, list[Candidate]] = {}
        for row, label in enumerate(labels[: len(scores)]):
            box: BoxXyxy = (
                float(boxes[row, 0]),
                float(boxes[row, 1]),
                float(boxes[row, 2]),
                float(boxes[row, 3]),
            )
            if masks is not None and row < len(masks):
                bounds = self._mask_box(masks[row])
                if bounds is not None:
                    box = bounds
            found.setdefault(str(label), []).append(Candidate(box=box, score=float(scores[row])))
        return found

    def _mask_box(self, mask: Any) -> BoxXyxy | None:
        """Speckle-free mask bounds, or ``None`` for an empty or odd mask."""
        np, cv2 = self._m["np"], self._m["cv2"]
        binary = np.ascontiguousarray(mask, dtype=np.uint8)
        if binary.ndim != 2 or binary.size == 0:
            return None
        count, _, stats, _ = cv2.connectedComponentsWithStats(binary, connectivity=8)
        components = [
            (int(row[0]), int(row[1]), int(row[2]), int(row[3]), int(row[4]))
            for row in stats[1 : int(count)]
        ]
        return speckle_bounds(components, int(binary.shape[1]), int(binary.shape[0]))


def configure_resolution(model: Any, processor: Any, side: int) -> int:
    """Run image detection at ``side`` px; returns the patch grid size.

    Ported from the PoC's ``sam31_resolution.configure_resolution``: the same
    side check, and the processor's square size. The tracker lines are left
    out because image detection never uses the tracker (see module docs).
    """
    patch = int(model.detector_model.vision_encoder.backbone.config.patch_size)
    check_side(side, patch)
    processor.image_size = side
    return side // patch
