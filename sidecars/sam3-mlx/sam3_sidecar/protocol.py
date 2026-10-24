"""Sidecar protocol v1: framing, message validation and message builders.

Theory
------
The Rust host and this child talk over two pipes: the host writes to the
child's stdin and reads the child's stdout. A pipe is a byte stream, so every
message is a self-delimiting frame::

    u32 little-endian header_len | header_len bytes of UTF-8 JSON (the header)
    u32 little-endian payload_len | payload_len bytes (the payload)

The header carries the message; the payload carries bulk bytes (RGB8 pixels in
a ``detect``; empty in every child-to-host message of v1, where masks are
reserved for later). Keeping pixels out of JSON avoids base64 and lets the
child read a frame with two length-prefixed reads.

Messages (both sides must match exactly):

* child -> host ``hello``, once, after the model has loaded (payload empty).
* host -> child ``detect`` with ``width * height * 3`` RGB8 bytes, row-major,
  top row first; phrases each carry their own score threshold.
* child -> host ``result`` echoing the request's ``id``, ``source``, ``seq``
  and ``pts_ns``, with boxes normalized to 0..1 of the image sent (top-left
  origin, ``[x, y, w, h]``) and only detections at or above their phrase's
  threshold.
* child -> host ``error``; ``id`` null means fatal and the child exits right
  after sending it.

Invariants
----------
* A header is at most ``HEADER_LIMIT`` bytes and a payload at most
  ``PAYLOAD_LIMIT`` bytes; a larger length prefix is a protocol error, checked
  before any of the body is read, so a corrupt prefix never allocates.
* End of stream exactly at a frame boundary is a clean shutdown; anywhere else
  it is a protocol error (a truncated frame).
* Two error classes keep the stream honest. ``ProtocolError`` means the byte
  stream or a header can no longer be trusted (bad lengths, bad JSON, no
  usable id): the child sends a fatal error and exits. ``RequestError`` means
  one well-framed request is invalid: the child answers that id with an error
  and carries on, because the framing is still in sync.
* Integers are checked to their wire width (u32, u64) and booleans are never
  accepted as numbers, although Python's ``bool`` is an ``int``.
* JSON never carries NaN or infinities: decoding rejects them and encoding
  refuses them (``allow_nan=False``).
* Unknown header fields are ignored, so later protocol additions do not break
  this child.

This module is stdlib only (``struct`` and ``json``) so the codec and the fake
backend run anywhere.
"""

from __future__ import annotations

import json
import math
import struct
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Any, Protocol, TypeGuard

#: Protocol version this child speaks, sent in ``hello``.
PROTOCOL_VERSION = 1

#: Largest accepted header, in bytes (1 MiB, inclusive).
HEADER_LIMIT = 1 << 20

#: Largest accepted payload, in bytes (64 MiB, inclusive).
PAYLOAD_LIMIT = 64 << 20

#: Largest value of a wire u32.
U32_MAX = (1 << 32) - 1

#: Largest value of a wire u64.
U64_MAX = (1 << 64) - 1

#: The only pixel format of protocol v1.
PIXEL_FORMAT = "rgb8"

#: Bytes per pixel of ``PIXEL_FORMAT``.
BYTES_PER_PIXEL = 3

#: The u32 little-endian length prefix used for both header and payload.
_LENGTH = struct.Struct("<I")


class Reader(Protocol):
    """A blocking binary input, such as stdin's buffer or ``io.BytesIO``."""

    def readinto(self, buffer: memoryview, /) -> int | None:
        """Fill ``buffer`` with up to its length; return the count, 0 at EOF."""


class Writer(Protocol):
    """A binary output, such as the protocol stdout or ``io.BytesIO``."""

    def write(self, data: bytes, /) -> Any:
        """Write all of ``data`` (buffered streams never write short)."""

    def flush(self) -> Any:
        """Push buffered bytes to the host now."""


class ProtocolError(Exception):
    """The stream or a header cannot be trusted; the child must stop.

    Raised for bad length prefixes, truncated frames, undecodable headers and
    requests without a usable id. The caller answers with a fatal error
    (``id`` null) and exits.
    """


class RequestError(Exception):
    """One well-framed request is invalid; answer its id and carry on."""

    def __init__(self, request_id: int, message: str) -> None:
        """Remember which request failed, and why.

        ``request_id`` is the request's validated u64 id; ``message`` is the
        human-readable reason sent back in the error header.
        """
        super().__init__(message)
        #: The id of the request this error answers.
        self.request_id = request_id


@dataclass(frozen=True)
class Frame:
    """One decoded frame: a JSON object header and its raw payload."""

    #: The decoded header; always a JSON object.
    header: dict[str, Any]
    #: The payload bytes, possibly empty.
    payload: bytes


@dataclass(frozen=True)
class Phrase:
    """One phrase of a ``detect`` request."""

    #: The text sent to the model, exactly as the host wrote it.
    text: str
    #: Keep only detections scoring at or above this, in 0..1.
    threshold: float


@dataclass(frozen=True)
class DetectRequest:
    """A validated ``detect`` request; its pixels match its size exactly."""

    #: Request id, echoed in the reply.
    id: int
    #: Source id, echoed in the reply.
    source: str
    #: Source frame sequence number, echoed in the reply.
    seq: int
    #: Source frame presentation time in nanoseconds, echoed in the reply.
    pts_ns: int
    #: Image width in pixels, at least 1.
    width: int
    #: Image height in pixels, at least 1.
    height: int
    #: The phrases to find, in host order; replies index into this.
    phrases: tuple[Phrase, ...]
    #: ``width * height * 3`` RGB8 bytes, row-major, top row first.
    pixels: bytes


@dataclass(frozen=True)
class Detection:
    """One reply detection, already filtered by its phrase's threshold."""

    #: Index into the request's ``phrases``.
    phrase: int
    #: ``(x, y, w, h)`` normalized to 0..1 of the image sent, top-left origin.
    box: tuple[float, float, float, float]
    #: Model score in 0..1.
    score: float


def _read_exact(stream: Reader, size: int) -> bytes | None:
    """Read exactly ``size`` bytes, or return what EOF left as a short result.

    Returns ``None`` only when EOF arrives before the first byte; a partial
    read returns the shorter bytes so the caller can tell a clean boundary
    from a truncated frame. Loops because a pipe may deliver a frame in
    pieces.
    """
    if size == 0:
        return b""
    buffer = bytearray(size)
    view = memoryview(buffer)
    filled = 0
    while filled < size:
        count = stream.readinto(view[filled:])
        if not count:
            break
        filled += count
    if filled == 0:
        return None
    return bytes(buffer[:filled])


def _read_length(stream: Reader, what: str, *, at_boundary: bool) -> int | None:
    """Read one u32 LE length prefix.

    At a frame boundary (``at_boundary``) EOF before the first byte returns
    ``None``; any other short read is a truncated frame.
    """
    raw = _read_exact(stream, _LENGTH.size)
    if raw is None:
        if at_boundary:
            return None
        raise ProtocolError(f"stream ended before the {what} length")
    if len(raw) < _LENGTH.size:
        raise ProtocolError(f"stream ended inside the {what} length")
    return int(_LENGTH.unpack(raw)[0])


def _read_body(stream: Reader, size: int, what: str) -> bytes:
    """Read a body of exactly ``size`` bytes or fail as a truncated frame."""
    body = _read_exact(stream, size)
    if body is None or len(body) < size:
        got = 0 if body is None else len(body)
        raise ProtocolError(f"stream ended inside the {what} ({got} of {size} bytes)")
    return body


def _reject_constant(name: str) -> Any:
    """Refuse JSON's non-standard NaN and infinity literals."""
    raise ValueError(f"non-finite number {name} is not valid in a header")


def decode_header(raw: bytes) -> dict[str, Any]:
    """Decode a header body into a JSON object or raise ``ProtocolError``."""
    try:
        text = raw.decode("utf-8")
        header = json.loads(text, parse_constant=_reject_constant)
    except (UnicodeDecodeError, ValueError) as exc:
        raise ProtocolError(f"header is not valid UTF-8 JSON: {exc}") from exc
    if not isinstance(header, dict):
        raise ProtocolError("header must be a JSON object")
    return header


def read_frame(stream: Reader) -> Frame | None:
    """Read one frame; ``None`` on a clean EOF at a frame boundary.

    Raises ``ProtocolError`` for an over-limit length (before reading the
    body), a truncated frame or an undecodable header.
    """
    header_len = _read_length(stream, "header", at_boundary=True)
    if header_len is None:
        return None
    if header_len > HEADER_LIMIT:
        raise ProtocolError(f"header of {header_len} bytes exceeds the {HEADER_LIMIT} byte limit")
    header = decode_header(_read_body(stream, header_len, "header"))
    payload_len = _read_length(stream, "payload", at_boundary=False)
    assert payload_len is not None  # at_boundary=False never returns None
    if payload_len > PAYLOAD_LIMIT:
        raise ProtocolError(
            f"payload of {payload_len} bytes exceeds the {PAYLOAD_LIMIT} byte limit"
        )
    payload = _read_body(stream, payload_len, "payload")
    return Frame(header=header, payload=payload)


def encode_frame(header: Mapping[str, Any], payload: bytes = b"") -> bytes:
    """Encode one frame; raise ``ProtocolError`` if it would break a limit.

    The header is compact JSON with non-ASCII kept as UTF-8. Non-finite floats
    are refused rather than written as invalid JSON.
    """
    try:
        text = json.dumps(header, ensure_ascii=False, allow_nan=False, separators=(",", ":"))
    except (TypeError, ValueError) as exc:
        raise ProtocolError(f"header cannot be encoded as JSON: {exc}") from exc
    raw = text.encode("utf-8")
    if len(raw) > HEADER_LIMIT:
        raise ProtocolError(f"header of {len(raw)} bytes exceeds the {HEADER_LIMIT} byte limit")
    if len(payload) > PAYLOAD_LIMIT:
        raise ProtocolError(
            f"payload of {len(payload)} bytes exceeds the {PAYLOAD_LIMIT} byte limit"
        )
    return b"".join((_LENGTH.pack(len(raw)), raw, _LENGTH.pack(len(payload)), payload))


def write_frame(stream: Writer, header: Mapping[str, Any], payload: bytes = b"") -> None:
    """Encode one frame, write it in one call and flush it to the host."""
    stream.write(encode_frame(header, payload))
    stream.flush()


def _is_int(value: object) -> TypeGuard[int]:
    """Tell whether ``value`` is a JSON integer (``bool`` is not, though an ``int``)."""
    return isinstance(value, int) and not isinstance(value, bool)


def _is_number(value: object) -> TypeGuard[float]:
    """Tell whether ``value`` is a finite JSON number (never a ``bool``)."""
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return False
    return math.isfinite(value)


def request_id(header: Mapping[str, Any]) -> int | None:
    """Return the header's ``id`` when it is a valid u64, else ``None``."""
    value = header.get("id")
    if _is_int(value) and 0 <= value <= U64_MAX:
        return int(value)
    return None


def _field_u64(header: Mapping[str, Any], name: str, rid: int) -> int:
    """Read a required u64 field or raise ``RequestError`` for ``rid``."""
    value = header.get(name)
    if not _is_int(value) or not 0 <= value <= U64_MAX:
        raise RequestError(rid, f"{name} must be an integer in 0..2^64-1")
    return int(value)


def _field_dimension(header: Mapping[str, Any], name: str, rid: int) -> int:
    """Read a required image dimension: a u32 of at least 1."""
    value = header.get(name)
    if not _is_int(value) or not 1 <= value <= U32_MAX:
        raise RequestError(rid, f"{name} must be an integer in 1..2^32-1")
    return int(value)


def _parse_phrases(value: Any, rid: int) -> tuple[Phrase, ...]:
    """Validate the ``phrases`` array: objects with text and a 0..1 threshold."""
    if not isinstance(value, list):
        raise RequestError(rid, "phrases must be an array")
    phrases: list[Phrase] = []
    for index, item in enumerate(value):
        if not isinstance(item, dict):
            raise RequestError(rid, f"phrases[{index}] must be an object")
        text = item.get("text")
        if not isinstance(text, str) or not text.strip():
            raise RequestError(rid, f"phrases[{index}].text must be a non-empty string")
        threshold = item.get("threshold")
        if not _is_number(threshold) or not 0.0 <= threshold <= 1.0:
            raise RequestError(rid, f"phrases[{index}].threshold must be a number in 0..1")
        phrases.append(Phrase(text=text, threshold=float(threshold)))
    return tuple(phrases)


def parse_detect(frame: Frame) -> DetectRequest:
    """Validate a host frame as a ``detect`` request.

    Raises ``ProtocolError`` when the header has no valid u64 ``id`` (nothing
    can be answered), otherwise ``RequestError`` naming that id for any other
    problem: an unknown message type, a missing or mistyped field, a format
    other than ``rgb8``, a payload whose size is not ``width * height * 3``,
    or ``want_masks`` set (v1 has no masks; the field may be omitted and then
    means false).
    """
    header = frame.header
    rid = request_id(header)
    if rid is None:
        raise ProtocolError("message has no valid u64 id")
    kind = header.get("type")
    if kind != "detect":
        raise RequestError(rid, f"unsupported message type {kind!r}; protocol v1 has only detect")
    source = header.get("source")
    if not isinstance(source, str) or not source:
        raise RequestError(rid, "source must be a non-empty string")
    seq = _field_u64(header, "seq", rid)
    pts_ns = _field_u64(header, "pts_ns", rid)
    width = _field_dimension(header, "width", rid)
    height = _field_dimension(header, "height", rid)
    if header.get("format") != PIXEL_FORMAT:
        raise RequestError(rid, f"format must be {PIXEL_FORMAT!r}")
    want_masks = header.get("want_masks", False)
    if not isinstance(want_masks, bool):
        raise RequestError(rid, "want_masks must be a boolean")
    if want_masks:
        raise RequestError(rid, "want_masks is true but this child has no masks")
    phrases = _parse_phrases(header.get("phrases"), rid)
    expected = width * height * BYTES_PER_PIXEL
    if len(frame.payload) != expected:
        raise RequestError(
            rid,
            f"payload is {len(frame.payload)} bytes; {width}x{height} rgb8 needs {expected}",
        )
    return DetectRequest(
        id=rid,
        source=source,
        seq=seq,
        pts_ns=pts_ns,
        width=width,
        height=height,
        phrases=phrases,
        pixels=frame.payload,
    )


def hello_header(model: str, weights_sha256: str | None, input_side: int) -> dict[str, Any]:
    """Build the ``hello`` header sent once the model is ready.

    Capabilities are fixed for v1: no masks and no parts.
    """
    return {
        "type": "hello",
        "protocol": PROTOCOL_VERSION,
        "model": model,
        "weights_sha256": weights_sha256,
        "input_side": input_side,
        "capabilities": {"masks": False, "parts": False},
    }


def result_header(
    request: DetectRequest, latency_ms: float, detections: Sequence[Detection]
) -> dict[str, Any]:
    """Build the ``result`` header answering ``request`` (payload stays empty)."""
    return {
        "type": "result",
        "id": request.id,
        "source": request.source,
        "seq": request.seq,
        "pts_ns": request.pts_ns,
        "latency_ms": float(latency_ms),
        "detections": [
            {"phrase": d.phrase, "box": [float(v) for v in d.box], "score": float(d.score)}
            for d in detections
        ],
    }


def error_header(request_id_or_none: int | None, message: str) -> dict[str, Any]:
    """Build an ``error`` header; ``None`` as the id marks it fatal."""
    return {"type": "error", "id": request_id_or_none, "message": message}
