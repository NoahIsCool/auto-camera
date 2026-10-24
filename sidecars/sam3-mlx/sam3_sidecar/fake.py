"""A deterministic stand-in for SAM 3.1, for tests on any machine.

Theory
------
The fake exercises everything except the model: the protocol, phrase
deduplication, thresholds and the error path. Its answer depends only on the
request, so a host test can predict it exactly without hashing or images:

* the k-th distinct text of a request (0-based, first-seen order) yields one
  box ``(x0, y0, x1, y1) = (0.05 + 0.1 * (k % 9), 0.2, 0.13 + 0.1 * (k % 9),
  0.8)``, that is ``[0.05 + 0.1 * (k % 9), 0.2, 0.08, 0.6]`` as xywh;
* every box scores ``FAKE_SCORE`` (0.9), so a phrase threshold above 0.9
  drops it and anything at or below keeps it;
* the text ``FAIL_PHRASE`` makes the request raise, to test that the child
  answers that id with an error and carries on.

Invariants
----------
* No third-party imports and no clocks, files or randomness.
* ``hello`` reports model ``"fake"`` and a null weights digest.
"""

from __future__ import annotations

from collections.abc import Mapping, Sequence

from .backend import Backend, Candidate
from .protocol import DetectRequest

#: Score of every fake detection.
FAKE_SCORE = 0.9

#: A phrase with this text makes the request fail with a ``RuntimeError``.
FAIL_PHRASE = "fake:error"

#: Model name the fake reports in ``hello``.
FAKE_MODEL = "fake"


class FakeBackend(Backend):
    """Deterministic boxes: one per distinct text, placed by its order."""

    def __init__(self, input_side: int) -> None:
        """Pretend to run at ``input_side``; nothing is loaded."""
        super().__init__(model=FAKE_MODEL, weights_sha256=None, input_side=input_side)

    def find(
        self, request: DetectRequest, texts: Sequence[str], floor: float
    ) -> Mapping[str, Sequence[Candidate]]:
        """Place one box per text; ``FAIL_PHRASE`` raises instead."""
        found: dict[str, list[Candidate]] = {}
        for k, text in enumerate(texts):
            if text == FAIL_PHRASE:
                raise RuntimeError("fake backend was asked to fail")
            x0 = 0.05 + 0.1 * (k % 9)
            if FAKE_SCORE >= floor:
                found[text] = [Candidate(box=(x0, 0.2, x0 + 0.08, 0.8), score=FAKE_SCORE)]
        return found
