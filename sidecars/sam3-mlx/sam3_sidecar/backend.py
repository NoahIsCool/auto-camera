"""The backend interface and the pure geometry and bookkeeping behind it.

Theory
------
A backend turns one RGB frame and a set of text phrases into scored boxes.
Requests may repeat a text with different thresholds (two queries sharing a
phrase), so ``Backend.detect`` is a template method:

1. ``unique_texts`` collapses the phrases to each distinct text, in first-seen
   order, with the lowest threshold any phrase asks for it;
2. the backend's ``find`` runs the model once per distinct text and returns
   every candidate scoring at or above that lowest threshold (the floor);
3. ``fan_out`` gives each phrase index the candidates at or above its own
   threshold, so a duplicated text is answered once per index.

Per-text greedy NMS commutes with this split: a box is only suppressed by a
higher-scoring box, which is above any threshold the suppressed box passes,
so filtering a superset after NMS equals NMS on the filtered set.

Boxes travel as ``(x0, y0, x1, y1)`` normalized to the image sent. The model
sees a square squash of that image, and a squash scales each axis on its own,
so a box normalized in the square is already normalized in the original.

Masks, when a backend has them, give tighter and more honest bounds than the
detector's box head, but a mask can carry isolated speckles far from the
subject. ``speckle_bounds`` is the proof of concept's 3% rule
(``auto_camera/mask_bounds.py`` ``component_bounds``): connected components
smaller than 3% of the largest are ignored and the box spans the rest.

Invariants
----------
* Every emitted box lies in 0..1 with positive width and height, and every
  score is finite in 0..1; anything else is dropped, never clamped into a
  fake detection.
* Emitted detections are ordered by phrase index, then by falling score.
* ``PhraseUse`` keeps the texts of each source's latest request for at most
  ``MAX_TRACKED_SOURCES`` sources, so a text-embedding cache pruned to
  ``in_use()`` is bounded by what the studio currently asks, not by history.

Stdlib only; the MLX backend supplies its components from OpenCV.
"""

from __future__ import annotations

import abc
import math
from collections import OrderedDict
from collections.abc import Iterable, Mapping, MutableMapping, Sequence
from collections.abc import Set as AbstractSet
from dataclasses import dataclass

from .protocol import Detection, DetectRequest, Phrase

#: Components smaller than this share of the largest are speckles (PoC rule).
SPECKLE_RELATIVE_AREA = 0.03

#: How many sources' latest phrase sets ``PhraseUse`` remembers.
MAX_TRACKED_SOURCES = 32

#: Smallest square side the model may be configured to (PoC bound).
MIN_SIDE = 224

#: Largest square side the model may be configured to (PoC bound).
MAX_SIDE = 1008

#: ``(x0, y0, x1, y1)`` normalized to the image sent.
BoxXyxy = tuple[float, float, float, float]

#: ``(x, y, w, h)`` normalized to the image sent.
BoxXywh = tuple[float, float, float, float]

#: One connected component: ``(left, top, width, height, area)`` in pixels.
Component = tuple[int, int, int, int, int]


@dataclass(frozen=True)
class Candidate:
    """One model detection for one text, before per-phrase thresholds."""

    #: ``(x0, y0, x1, y1)`` normalized to the image sent; may need clipping.
    box: BoxXyxy
    #: Model score; expected in 0..1.
    score: float


class Backend(abc.ABC):
    """A loaded detector that answers ``detect`` requests one at a time.

    Subclasses set the ``hello`` facts at construction and implement
    ``find``; ``detect`` handles phrase deduplication and thresholds.
    """

    def __init__(self, model: str, weights_sha256: str | None, input_side: int) -> None:
        """Record the facts the ``hello`` message reports."""
        #: Model name reported in ``hello``.
        self.model = model
        #: Weights digest reported in ``hello``, or ``None`` when unknown.
        self.weights_sha256 = weights_sha256
        #: Square side the model runs at, reported in ``hello``.
        self.input_side = input_side

    def detect(self, request: DetectRequest) -> list[Detection]:
        """Answer one request: every phrase's detections at its threshold."""
        texts = unique_texts(request.phrases)
        if not texts:
            return []
        found = self.find(request, list(texts), min(texts.values()))
        return fan_out(request.phrases, found)

    @abc.abstractmethod
    def find(
        self, request: DetectRequest, texts: Sequence[str], floor: float
    ) -> Mapping[str, Sequence[Candidate]]:
        """Run the model once per distinct text on the request's pixels.

        Returns, per text, the candidates scoring at or above ``floor`` (the
        lowest threshold in the request); texts without matches may be absent.
        Raising fails only this request.
        """


def unique_texts(phrases: Sequence[Phrase]) -> dict[str, float]:
    """Map each distinct text to its lowest threshold, in first-seen order."""
    texts: dict[str, float] = {}
    for phrase in phrases:
        current = texts.get(phrase.text)
        texts[phrase.text] = phrase.threshold if current is None else min(current, phrase.threshold)
    return texts


def clip_box(box: BoxXyxy) -> BoxXywh | None:
    """Clip a normalized corner box to the image and convert it to xywh.

    Returns ``None`` for a non-finite box or one with no area inside the
    image.
    """
    if len(box) != 4 or not all(math.isfinite(v) for v in box):
        return None
    x0, y0, x1, y1 = (min(max(float(v), 0.0), 1.0) for v in box)
    width, height = x1 - x0, y1 - y0
    if width <= 0.0 or height <= 0.0:
        return None
    return (x0, y0, width, height)


def fan_out(phrases: Sequence[Phrase], found: Mapping[str, Sequence[Candidate]]) -> list[Detection]:
    """Give each phrase index its candidates at or above its own threshold.

    Candidates with an invalid box or a score outside 0..1 are dropped.
    """
    detections: list[Detection] = []
    for index, phrase in enumerate(phrases):
        ranked = sorted(found.get(phrase.text, ()), key=lambda c: -c.score)
        for candidate in ranked:
            score = float(candidate.score)
            if not math.isfinite(score) or not 0.0 <= score <= 1.0:
                continue
            if score < phrase.threshold:
                continue
            box = clip_box(candidate.box)
            if box is not None:
                detections.append(Detection(phrase=index, box=box, score=score))
    return detections


def speckle_bounds(
    components: Iterable[Component],
    width: int,
    height: int,
    minimum_relative_area: float = SPECKLE_RELATIVE_AREA,
) -> BoxXyxy | None:
    """Bounds of a mask's components, ignoring speckles (the PoC 3% rule).

    ``components`` are the foreground components of a ``width`` x ``height``
    mask, background excluded, as OpenCV's ``connectedComponentsWithStats``
    reports them. Components with an area below ``minimum_relative_area``
    times the largest are ignored; the result spans the rest, normalized with
    exclusive right and bottom edges. ``None`` when the mask is empty.
    """
    if width < 1 or height < 1:
        raise ValueError("mask dimensions must be positive")
    if not 0.0 < minimum_relative_area <= 1.0:
        raise ValueError("relative area must be in (0, 1]")
    solid = [c for c in components if c[4] > 0]
    if not solid:
        return None
    largest = max(c[4] for c in solid)
    kept = [c for c in solid if c[4] >= largest * minimum_relative_area]
    left = min(c[0] for c in kept)
    top = min(c[1] for c in kept)
    right = max(c[0] + c[2] for c in kept)
    bottom = max(c[1] + c[3] for c in kept)
    return (left / width, top / height, right / width, bottom / height)


def check_side(side: int, patch: int) -> None:
    """Validate a model side like the PoC's ``configure_resolution``.

    The side must lie in ``MIN_SIDE..MAX_SIDE`` and be a multiple of twice
    the ViT patch size, so the patch grid is even.
    """
    if patch < 1:
        raise ValueError("patch size must be positive")
    if not MIN_SIDE <= side <= MAX_SIDE or side % (2 * patch):
        raise ValueError(f"side must be a multiple of {2 * patch} in [{MIN_SIDE}, {MAX_SIDE}]")


class PhraseUse:
    """Which texts the studio currently asks for, across sources.

    Each source's latest request replaces its own set; the least recently
    heard source is forgotten once more than ``limit`` are tracked. A cache
    pruned to ``in_use()`` keeps every live source's embeddings warm while
    retired prompts are freed.
    """

    def __init__(self, limit: int = MAX_TRACKED_SOURCES) -> None:
        """Track at most ``limit`` sources (at least one)."""
        if limit < 1:
            raise ValueError("limit must be at least 1")
        #: Most sources remembered at once.
        self._limit = limit
        #: Source id to the texts of its latest request, oldest first.
        self._latest: OrderedDict[str, frozenset[str]] = OrderedDict()

    def record(self, source: str, texts: Iterable[str]) -> None:
        """Remember ``texts`` as ``source``'s current phrase set."""
        self._latest[source] = frozenset(texts)
        self._latest.move_to_end(source)
        while len(self._latest) > self._limit:
            self._latest.popitem(last=False)

    def in_use(self) -> frozenset[str]:
        """Union of every tracked source's latest texts."""
        result: frozenset[str] = frozenset()
        for texts in self._latest.values():
            result |= texts
        return result


def prune_cache(cache: MutableMapping[str, object], keep: AbstractSet[str]) -> int:
    """Delete cache entries whose key is not in ``keep``; return how many.

    The same job as the PoC's ``prune_prompt_cache`` on mlx-vlm's
    ``Sam3Predictor._text_cache``, which otherwise keeps every embedding it
    ever computed.
    """
    stale = [key for key in cache if key not in keep]
    for key in stale:
        del cache[key]
    return len(stale)
