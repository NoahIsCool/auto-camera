"""The child's request loop: load, say hello, answer until stdin ends.

Theory
------
The child is stateless between requests apart from caches, and the host
keeps at most one request in flight, so the loop is strictly sequential:
read a frame, validate it, run the backend, write exactly one reply. That
gives the host a simple contract:

* the first frame is ``hello`` (the model is loaded and warmed up) or a fatal
  error (it failed to load);
* every ``detect`` gets exactly one reply with its id: a ``result`` or an
  ``error``; a failing request never takes the child down;
* a frame the child cannot trust gets a fatal error (``id`` null), after
  which the child exits, because the stream is no longer in sync;
* end of stdin at a frame boundary is the shutdown signal: the child exits
  with ``EXIT_OK`` and writes nothing more.

Exit codes (``1`` stays Python's own crash code and ``2`` is a bad command
line, reported by the server):

* ``EXIT_OK`` (0): stdin ended cleanly;
* ``EXIT_LOAD_FAILED`` (3): the backend failed to load; a fatal error was sent;
* ``EXIT_PROTOCOL`` (4): the host broke the protocol; a fatal error was sent;
* ``EXIT_HOST_GONE`` (5): stdout closed (the host went away) mid-reply.

Invariants
----------
* Nothing but frames is written to ``writer``; logs go to the ``logging``
  tree, which the server sends to stderr.
* ``latency_ms`` is the backend's time for the request (pixel decode,
  preprocessing, model and post-processing) on a monotonic clock; pipe
  transfer is not included.
* ``KeyboardInterrupt`` and ``SystemExit`` are not caught here.
"""

from __future__ import annotations

import logging
import time
from collections.abc import Callable
from typing import Any

from .backend import Backend
from .protocol import (
    DetectRequest,
    ProtocolError,
    Reader,
    RequestError,
    Writer,
    encode_frame,
    error_header,
    hello_header,
    parse_detect,
    read_frame,
    result_header,
)

#: stdin ended cleanly at a frame boundary.
EXIT_OK = 0

#: The backend failed to load; a fatal error was sent.
EXIT_LOAD_FAILED = 3

#: The host broke the protocol; a fatal error was sent.
EXIT_PROTOCOL = 4

#: stdout was closed while replying; the host is gone.
EXIT_HOST_GONE = 5

#: Longest error message sent to the host, in characters.
MESSAGE_LIMIT = 2000

#: Logger for the loop; the server routes it to stderr.
_LOG = logging.getLogger("sam3_sidecar.loop")


class _HostGoneError(Exception):
    """Writing to the host failed; stop without another write."""


def bounded(text: str) -> str:
    """Fold ``text`` onto one line of at most ``MESSAGE_LIMIT`` characters.

    Error replies echo host input (an unknown type, a bad field), so every
    message is bounded before encoding and can never break the header limit.
    """
    text = " ".join(text.split())
    if len(text) > MESSAGE_LIMIT:
        text = text[: MESSAGE_LIMIT - 3] + "..."
    return text


def describe(exc: BaseException) -> str:
    """Describe an exception in one bounded line, for an error reply."""
    return bounded(f"{type(exc).__name__}: {exc}")


def _error_frame(request_id: int | None, message: str) -> bytes:
    """Encode an error reply with a bounded message."""
    return encode_frame(error_header(request_id, bounded(message)))


def _send(writer: Writer, frame: bytes) -> None:
    """Write one encoded frame and flush; raise ``_HostGoneError`` on failure."""
    try:
        writer.write(frame)
        writer.flush()
    except (BrokenPipeError, ConnectionResetError, ValueError, OSError) as exc:
        raise _HostGoneError(describe(exc)) from exc


def send_fatal(writer: Writer, message: str) -> None:
    """Best-effort fatal error (``id`` null); a host already gone is ignored.

    The caller exits right after, as the protocol requires.
    """
    _LOG.error("fatal: %s", message)
    try:
        _send(writer, _error_frame(None, message))
    except _HostGoneError:
        _LOG.warning("could not report the fatal error: the host is gone")


def serve(
    reader: Reader,
    writer: Writer,
    load: Callable[[], Backend],
    clock: Callable[[], float] = time.perf_counter,
) -> int:
    """Run the child: load the backend, send ``hello``, answer requests.

    ``load`` builds the backend (any exception is a fatal load failure);
    ``clock`` is the monotonic clock in seconds used for ``latency_ms``.
    Returns one of the module's exit codes.
    """
    try:
        backend = load()
    except Exception as exc:  # noqa: BLE001 - any load failure is fatal
        _LOG.exception("backend failed to load")
        send_fatal(writer, f"model failed to load: {describe(exc)}")
        return EXIT_LOAD_FAILED
    try:
        _send(
            writer,
            encode_frame(hello_header(backend.model, backend.weights_sha256, backend.input_side)),
        )
        _LOG.info("ready: %s at %d px", backend.model, backend.input_side)
        return _answer(reader, writer, backend, clock)
    except _HostGoneError as exc:
        _LOG.warning("host is gone: %s", exc)
        return EXIT_HOST_GONE


def _answer(reader: Reader, writer: Writer, backend: Backend, clock: Callable[[], float]) -> int:
    """Answer frames until EOF or a protocol error; see ``serve``."""
    while True:
        try:
            frame = read_frame(reader)
        except ProtocolError as exc:
            send_fatal(writer, f"protocol error: {exc}")
            return EXIT_PROTOCOL
        if frame is None:
            _LOG.info("stdin closed; exiting")
            return EXIT_OK
        try:
            request = parse_detect(frame)
        except RequestError as exc:
            _LOG.warning("request %d rejected: %s", exc.request_id, exc)
            _send(writer, _error_frame(exc.request_id, str(exc)))
            continue
        except ProtocolError as exc:
            send_fatal(writer, f"protocol error: {exc}")
            return EXIT_PROTOCOL
        reply = _run(backend, request, clock)
        _send(writer, reply)


def _run(backend: Backend, request: DetectRequest, clock: Callable[[], float]) -> bytes:
    """Run one request and encode its reply; failures become its error."""
    started = clock()
    try:
        detections = backend.detect(request)
        latency_ms = (clock() - started) * 1000.0
        header: dict[str, Any] = result_header(request, latency_ms, detections)
        frame = encode_frame(header)
    except Exception as exc:  # noqa: BLE001 - one request must not end the child
        _LOG.exception("request %d failed", request.id)
        return _error_frame(request.id, describe(exc))
    _LOG.debug(
        "request %d source %s seq %d: %d phrases, %d detections, %.1f ms",
        request.id,
        request.source,
        request.seq,
        len(request.phrases),
        len(detections),
        latency_ms,
    )
    return frame
