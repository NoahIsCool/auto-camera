"""Codec, request validation, the request loop and the server process.

Covers normal traffic, invalid input (which must be answered or be fatal,
never crash) and edges: limits exactly at and just over, frames split into
single bytes, truncation at every offset, and EOF at a frame boundary.
"""

from __future__ import annotations

import io
import json
import os
import shlex
import signal
import struct
import subprocess
import sys
import time
import unittest
from collections.abc import Iterator
from typing import Any

from sam3_sidecar.backend import Backend
from sam3_sidecar.fake import FAIL_PHRASE, FAKE_SCORE, FakeBackend
from sam3_sidecar.loop import (
    EXIT_HOST_GONE,
    EXIT_LOAD_FAILED,
    EXIT_OK,
    EXIT_PROTOCOL,
    MESSAGE_LIMIT,
    describe,
    serve,
)
from sam3_sidecar.protocol import (
    HEADER_LIMIT,
    PAYLOAD_LIMIT,
    U32_MAX,
    U64_MAX,
    Detection,
    DetectRequest,
    Frame,
    Phrase,
    ProtocolError,
    RequestError,
    encode_frame,
    error_header,
    hello_header,
    parse_detect,
    read_frame,
    result_header,
    write_frame,
)

from .support import (
    CHILD_TIMEOUT_S,
    SERVER,
    detect_bytes,
    detect_header,
    parse_frames,
    pixels_for,
    run_python,
    run_server,
)


class TrickleReader:
    """A reader that hands out one byte per call, like a slow pipe."""

    def __init__(self, data: bytes) -> None:
        """Serve ``data`` one byte at a time."""
        #: The bytes still to deliver.
        self._data = data

    def readinto(self, buffer: memoryview, /) -> int:
        """Copy at most one byte into ``buffer``; 0 at the end."""
        if not self._data or not len(buffer):
            return 0
        buffer[0] = self._data[0]
        self._data = self._data[1:]
        return 1


class FramingTests(unittest.TestCase):
    """The length-prefixed frame codec."""

    def test_round_trip_header_and_payload(self) -> None:
        """A frame decodes to the header and payload that were encoded."""
        stream = io.BytesIO(encode_frame({"type": "x", "n": 3}, b"\x00\x01\x02"))
        frame = read_frame(stream)
        self.assertEqual(frame, Frame(header={"type": "x", "n": 3}, payload=b"\x00\x01\x02"))
        self.assertIsNone(read_frame(stream))

    def test_layout_is_two_little_endian_length_prefixes(self) -> None:
        """The bytes are u32 LE header_len, header, u32 LE payload_len, payload."""
        raw = encode_frame({"a": 1}, b"xy")
        self.assertEqual(raw, b'\x07\x00\x00\x00{"a":1}\x02\x00\x00\x00xy')

    def test_empty_stream_is_clean_eof(self) -> None:
        """EOF before any byte of a frame is a clean shutdown."""
        self.assertIsNone(read_frame(io.BytesIO(b"")))

    def test_consecutive_frames_then_eof(self) -> None:
        """Frames are read back in order until the stream ends."""
        data = encode_frame({"i": 1}) + encode_frame({"i": 2}, b"z") + encode_frame({"i": 3})
        frames = parse_frames(data)
        self.assertEqual([f.header["i"] for f in frames], [1, 2, 3])
        self.assertEqual([f.payload for f in frames], [b"", b"z", b""])

    def test_frame_delivered_one_byte_at_a_time(self) -> None:
        """A frame split into single-byte reads still decodes whole."""
        data = encode_frame({"type": "y"}, b"abc") + encode_frame({"type": "z"})
        reader = TrickleReader(data)
        first, second = read_frame(reader), read_frame(reader)
        self.assertEqual(first, Frame(header={"type": "y"}, payload=b"abc"))
        self.assertEqual(second, Frame(header={"type": "z"}, payload=b""))
        self.assertIsNone(read_frame(reader))

    def test_truncation_anywhere_inside_a_frame_is_a_protocol_error(self) -> None:
        """EOF at any offset other than a frame boundary is fatal."""
        full = encode_frame({"type": "x"}, b"payload")
        for cut in range(1, len(full)):
            with self.subTest(cut=cut), self.assertRaises(ProtocolError):
                read_frame(io.BytesIO(full[:cut]))

    def test_header_exactly_at_the_limit_is_accepted(self) -> None:
        """A header of exactly HEADER_LIMIT bytes is valid."""
        skeleton = len(json.dumps({"p": ""}, separators=(",", ":")).encode())
        header = {"p": "a" * (HEADER_LIMIT - skeleton)}
        frame = read_frame(io.BytesIO(encode_frame(header)))
        assert frame is not None
        self.assertEqual(len(frame.header["p"]), HEADER_LIMIT - skeleton)

    def test_oversized_header_is_refused_before_reading_it(self) -> None:
        """A header length over the limit fails without waiting for the body."""
        stream = io.BytesIO(struct.pack("<I", HEADER_LIMIT + 1))
        with self.assertRaisesRegex(ProtocolError, "exceeds"):
            read_frame(stream)

    def test_oversized_payload_is_refused_before_reading_it(self) -> None:
        """A payload length over the limit fails without waiting for the body."""
        stream = io.BytesIO(b"\x02\x00\x00\x00{}" + struct.pack("<I", PAYLOAD_LIMIT + 1))
        with self.assertRaisesRegex(ProtocolError, "exceeds"):
            read_frame(stream)

    def test_largest_length_prefix_is_refused(self) -> None:
        """A garbage prefix (0xffffffff) is refused, not allocated."""
        with self.assertRaisesRegex(ProtocolError, "exceeds"):
            read_frame(io.BytesIO(b"\xff\xff\xff\xff"))

    def test_header_must_be_a_utf8_json_object(self) -> None:
        """Bad UTF-8, bad JSON, non-objects, NaN and empty headers are fatal."""
        bodies = [b"\xff\xfe", b"{nope", b"[1,2]", b'"text"', b'{"x":NaN}', b'{"x":-Infinity}', b""]
        for body in bodies:
            raw = struct.pack("<I", len(body)) + body + struct.pack("<I", 0)
            with self.subTest(body=body), self.assertRaises(ProtocolError):
                read_frame(io.BytesIO(raw))

    def test_non_ascii_text_round_trips_as_utf8(self) -> None:
        """Phrases in any language survive the codec unchanged."""
        raw = encode_frame({"text": "café 人"})
        self.assertIn("café 人".encode(), raw)
        frame = read_frame(io.BytesIO(raw))
        assert frame is not None
        self.assertEqual(frame.header["text"], "café 人")

    def test_encoding_refuses_invalid_frames(self) -> None:
        """Non-finite floats, unencodable values and over-limit sizes fail."""
        for header in ({"x": float("nan")}, {"x": float("inf")}, {"x": object()}):
            with self.subTest(header=header), self.assertRaises(ProtocolError):
                encode_frame(header)
        with self.assertRaises(ProtocolError):
            encode_frame({"p": "a" * HEADER_LIMIT})
        with self.assertRaises(ProtocolError):
            encode_frame({}, bytes(PAYLOAD_LIMIT + 1))

    def test_write_frame_writes_and_flushes(self) -> None:
        """``write_frame`` writes one whole frame."""
        out = io.BytesIO()
        write_frame(out, {"type": "x"}, b"1")
        self.assertEqual(out.getvalue(), encode_frame({"type": "x"}, b"1"))


class ParseDetectTests(unittest.TestCase):
    """Validation of host ``detect`` requests."""

    def parse(self, header: dict[str, Any], payload: bytes | None = None) -> DetectRequest:
        """Parse ``header`` with matching pixels unless ``payload`` is given."""
        return parse_detect(
            Frame(header=header, payload=pixels_for(header) if payload is None else payload)
        )

    def assert_answers(
        self,
        header: dict[str, Any],
        request_id: int,
        payload: bytes | None = None,
        mentions: str = "",
    ) -> None:
        """Check the request is rejected with an error for ``request_id``.

        The error text must contain ``mentions``, so a case cannot pass by
        failing for some other reason.
        """
        with self.assertRaises(RequestError) as caught:
            self.parse(header, payload)
        self.assertEqual(caught.exception.request_id, request_id)
        self.assertIn(mentions, str(caught.exception))

    def test_valid_request(self) -> None:
        """Every field is carried over unchanged."""
        header = detect_header(
            9,
            phrases=[
                {"text": "a person speaking", "threshold": 0.3},
                {"text": "drummer", "threshold": 1},
            ],
        )
        request = self.parse(header)
        self.assertEqual(request.id, 9)
        self.assertEqual((request.source, request.seq, request.pts_ns), ("cam-1", 7, 1_000_000))
        self.assertEqual((request.width, request.height), (4, 2))
        self.assertEqual(
            request.phrases, (Phrase("a person speaking", 0.3), Phrase("drummer", 1.0))
        )
        self.assertEqual(request.pixels, pixels_for(header))

    def test_want_masks_may_be_omitted_and_unknown_fields_are_ignored(self) -> None:
        """Omitted want_masks means false; extra fields do not matter."""
        header = detect_header(2, future_field={"x": 1})
        del header["want_masks"]
        self.assertEqual(self.parse(header).id, 2)

    def test_empty_phrase_list_is_valid(self) -> None:
        """A request with no phrases is valid (and answered with nothing)."""
        self.assertEqual(self.parse(detect_header(3, phrases=[])).phrases, ())

    def test_wire_extremes_are_accepted(self) -> None:
        """u64 maxima for id, seq and pts_ns and a 1x1 frame are valid."""
        header = detect_header(U64_MAX, seq=U64_MAX, pts_ns=0, width=1, height=1)
        request = self.parse(header)
        self.assertEqual((request.id, request.seq, request.pts_ns), (U64_MAX, U64_MAX, 0))

    def test_request_without_a_valid_id_is_fatal(self) -> None:
        """No usable id means nothing can be answered: a protocol error."""
        for value in (None, -1, U64_MAX + 1, 1.0, True, "1", [1]):
            header = detect_header(value)
            with self.subTest(id=value), self.assertRaises(ProtocolError):
                self.parse(header)
        header = detect_header()
        del header["id"]
        with self.assertRaises(ProtocolError):
            self.parse(header)

    def test_unknown_type_is_answered(self) -> None:
        """A non-detect message with a valid id gets an error for that id."""
        for kind in ("ping", None, 5):
            with self.subTest(kind=kind):
                self.assert_answers(detect_header(4, type=kind), 4, mentions="type")

    def test_invalid_fields_are_answered(self) -> None:
        """Each malformed field is rejected with an error for the request's id."""
        cases: list[tuple[str, Any]] = [
            ("source", ""),
            ("source", 5),
            ("seq", -1),
            ("seq", U64_MAX + 1),
            ("seq", 1.5),
            ("seq", True),
            ("pts_ns", "0"),
            ("width", 0),
            ("width", -4),
            ("width", U32_MAX + 1),
            ("width", 4.0),
            ("height", False),
            ("format", "rgba8"),
            ("format", None),
            ("want_masks", True),
            ("want_masks", "no"),
            ("phrases", "person"),
            ("phrases", None),
            ("phrases", [1]),
            ("phrases", [{"text": "", "threshold": 0.5}]),
            ("phrases", [{"text": "   ", "threshold": 0.5}]),
            ("phrases", [{"text": 5, "threshold": 0.5}]),
            ("phrases", [{"threshold": 0.5}]),
            ("phrases", [{"text": "a", "threshold": -0.01}]),
            ("phrases", [{"text": "a", "threshold": 1.01}]),
            ("phrases", [{"text": "a", "threshold": True}]),
            ("phrases", [{"text": "a", "threshold": "0.5"}]),
            ("phrases", [{"text": "a"}]),
        ]
        default_pixels = pixels_for(detect_header())
        for name, value in cases:
            with self.subTest(field=name, value=value):
                header = detect_header(5, **{name: value})
                self.assert_answers(header, 5, payload=default_pixels, mentions=name)
        for name in ("source", "seq", "pts_ns", "width", "height", "format", "phrases"):
            header = detect_header(6)
            del header[name]
            with self.subTest(missing=name):
                self.assert_answers(header, 6, payload=default_pixels, mentions=name)

    def test_payload_must_be_exactly_width_by_height_rgb(self) -> None:
        """One byte too few or too many is rejected for that request."""
        header = detect_header(7)
        good = pixels_for(header)
        for payload in (good[:-1], good + b"\x00", b""):
            with self.subTest(size=len(payload)):
                self.assert_answers(header, 7, payload=payload, mentions="payload")


class MessageTests(unittest.TestCase):
    """Child-to-host message builders."""

    def test_hello_header(self) -> None:
        """``hello`` carries protocol 1, the model facts and no masks or parts."""
        self.assertEqual(
            hello_header("sam3.1-mlx", "ab" * 32, 672),
            {
                "type": "hello",
                "protocol": 1,
                "model": "sam3.1-mlx",
                "weights_sha256": "ab" * 32,
                "input_side": 672,
                "capabilities": {"masks": False, "parts": False},
            },
        )
        self.assertIsNone(hello_header("fake", None, 672)["weights_sha256"])

    def test_result_header_echoes_the_request(self) -> None:
        """``result`` repeats id, source, seq and pts_ns and lists detections."""
        request = parse_detect(
            Frame(detect_header(11, seq=3, pts_ns=99), pixels_for(detect_header()))
        )
        header = result_header(request, 12.5, [Detection(0, (0.1, 0.2, 0.3, 0.4), 0.75)])
        self.assertEqual(
            header,
            {
                "type": "result",
                "id": 11,
                "source": "cam-1",
                "seq": 3,
                "pts_ns": 99,
                "latency_ms": 12.5,
                "detections": [{"phrase": 0, "box": [0.1, 0.2, 0.3, 0.4], "score": 0.75}],
            },
        )

    def test_error_header(self) -> None:
        """``error`` carries the id, or null when fatal."""
        self.assertEqual(error_header(3, "bad"), {"type": "error", "id": 3, "message": "bad"})
        self.assertEqual(error_header(None, "dead")["id"], None)

    def test_describe_is_one_bounded_line(self) -> None:
        """Error text names the exception type and stays short."""
        self.assertEqual(describe(ValueError("two\nlines")), "ValueError: two lines")
        self.assertEqual(len(describe(RuntimeError("x" * 10_000))), MESSAGE_LIMIT)


class BrokenPipeWriter:
    """A writer whose host has gone away."""

    def write(self, data: bytes, /) -> int:
        """Fail like a closed pipe."""
        raise BrokenPipeError("host closed stdout")

    def flush(self) -> None:
        """Nothing to flush."""


def fake_loader() -> Backend:
    """Load the fake backend at the default side."""
    return FakeBackend(input_side=672)


def ticking_clock(step: float = 0.25) -> Iterator[float]:
    """Yield clock readings that advance by ``step`` seconds each."""
    now = 100.0
    while True:
        yield now
        now += step


class ServeTests(unittest.TestCase):
    """The request loop, in process, against the fake backend."""

    def serve(self, stdin: bytes, load: Any = fake_loader) -> tuple[int, list[Frame]]:
        """Run the loop over ``stdin``; return the exit code and reply frames.

        The loop's logs are captured (and so kept out of the test output).
        """
        out = io.BytesIO()
        clock = ticking_clock()
        with self.assertLogs("sam3_sidecar", level="DEBUG"):
            code = serve(io.BytesIO(stdin), out, load, clock=lambda: next(clock))
        return code, parse_frames(out.getvalue())

    def test_hello_then_result_then_clean_exit(self) -> None:
        """Hello comes first, each request gets its result, EOF exits cleanly."""
        header = detect_header(
            1,
            phrases=[
                {"text": "guitarist", "threshold": 0.5},
                {"text": "drummer", "threshold": 0.5},
            ],
        )
        code, frames = self.serve(detect_bytes(header))
        self.assertEqual(code, EXIT_OK)
        self.assertEqual([f.header["type"] for f in frames], ["hello", "result"])
        self.assertEqual(frames[0].header["model"], "fake")
        result = frames[1].header
        self.assertEqual(
            (result["id"], result["source"], result["seq"], result["pts_ns"]),
            (1, "cam-1", 7, 1_000_000),
        )
        self.assertEqual(result["latency_ms"], 250.0)
        self.assertEqual([d["phrase"] for d in result["detections"]], [0, 1])
        self.assertEqual([d["score"] for d in result["detections"]], [FAKE_SCORE, FAKE_SCORE])
        for k, detection in enumerate(result["detections"]):
            for got, want in zip(detection["box"], [0.05 + 0.1 * k, 0.2, 0.08, 0.6], strict=True):
                self.assertAlmostEqual(got, want)
        self.assertTrue(all(f.payload == b"" for f in frames))

    def test_no_requests_means_hello_only(self) -> None:
        """With stdin closed straight away the child says hello and exits."""
        code, frames = self.serve(b"")
        self.assertEqual(code, EXIT_OK)
        self.assertEqual([f.header["type"] for f in frames], ["hello"])

    def test_thresholds_are_inclusive_and_per_phrase(self) -> None:
        """A score equal to the threshold is kept; above it, dropped."""
        header = detect_header(
            2,
            phrases=[
                {"text": "person", "threshold": FAKE_SCORE},
                {"text": "person", "threshold": 0.95},
                {"text": "person", "threshold": 0.1},
            ],
        )
        _, frames = self.serve(detect_bytes(header))
        self.assertEqual([d["phrase"] for d in frames[1].header["detections"]], [0, 2])

    def test_empty_phrase_list_gets_an_empty_result(self) -> None:
        """No phrases, no detections, still one result."""
        _, frames = self.serve(detect_bytes(detect_header(3, phrases=[])))
        self.assertEqual(frames[1].header["type"], "result")
        self.assertEqual(frames[1].header["detections"], [])

    def test_bad_request_is_answered_and_the_loop_continues(self) -> None:
        """An invalid request gets its own error; the next one is served."""
        bad = detect_bytes(detect_header(1), payload=b"short")
        good = detect_bytes(detect_header(2))
        code, frames = self.serve(bad + good)
        self.assertEqual(code, EXIT_OK)
        self.assertEqual(
            [(f.header["type"], f.header.get("id")) for f in frames[1:]],
            [("error", 1), ("result", 2)],
        )
        self.assertIn("payload", frames[1].header["message"])

    def test_backend_failure_is_answered_and_the_loop_continues(self) -> None:
        """An exception inside the backend fails only its own request."""
        failing = detect_bytes(detect_header(5, phrases=[{"text": FAIL_PHRASE, "threshold": 0.5}]))
        good = detect_bytes(detect_header(6))
        code, frames = self.serve(failing + good)
        self.assertEqual(code, EXIT_OK)
        self.assertEqual(
            [(f.header["type"], f.header.get("id")) for f in frames[1:]],
            [("error", 5), ("result", 6)],
        )
        self.assertIn("RuntimeError", frames[1].header["message"])

    def test_error_replies_stay_bounded_when_echoing_host_input(self) -> None:
        """A huge unknown type is answered with a short error, then served on."""
        huge = detect_bytes(detect_header(8, type="x" * (HEADER_LIMIT - 1000)))
        code, frames = self.serve(huge + detect_bytes(detect_header(9)))
        self.assertEqual(code, EXIT_OK)
        self.assertEqual([f.header.get("id") for f in frames[1:]], [8, 9])
        self.assertLessEqual(len(frames[1].header["message"]), MESSAGE_LIMIT)

    def test_load_failure_is_fatal_and_no_hello_is_sent(self) -> None:
        """A backend that cannot load yields only a fatal error."""

        def broken() -> Backend:
            raise OSError("weights missing")

        code, frames = self.serve(detect_bytes(detect_header(1)), load=broken)
        self.assertEqual(code, EXIT_LOAD_FAILED)
        self.assertEqual(len(frames), 1)
        self.assertEqual(frames[0].header["type"], "error")
        self.assertIsNone(frames[0].header["id"])
        self.assertIn("weights missing", frames[0].header["message"])

    def test_garbage_after_hello_is_fatal(self) -> None:
        """An untrustworthy frame ends the child with a fatal error."""
        code, frames = self.serve(b"\xff\xff\xff\xff")
        self.assertEqual(code, EXIT_PROTOCOL)
        self.assertEqual(
            [(f.header["type"], f.header.get("id")) for f in frames],
            [("hello", None), ("error", None)],
        )

    def test_request_without_id_is_fatal_even_after_good_ones(self) -> None:
        """A good request is answered; one with no id is then fatal."""
        no_id = detect_header()
        del no_id["id"]
        code, frames = self.serve(
            detect_bytes(detect_header(1)) + detect_bytes(no_id) + detect_bytes(detect_header(3))
        )
        self.assertEqual(code, EXIT_PROTOCOL)
        self.assertEqual([f.header["type"] for f in frames], ["hello", "result", "error"])
        self.assertIsNone(frames[2].header["id"])

    def test_truncated_request_is_fatal(self) -> None:
        """EOF in the middle of a request is a protocol error."""
        code, frames = self.serve(detect_bytes(detect_header(1))[:-1])
        self.assertEqual(code, EXIT_PROTOCOL)
        self.assertEqual(frames[-1].header["type"], "error")

    def test_host_gone_stops_the_child(self) -> None:
        """A closed stdout ends the loop without raising."""
        with self.assertLogs("sam3_sidecar", level="WARNING") as logs:
            code = serve(io.BytesIO(b""), BrokenPipeWriter(), fake_loader)
        self.assertEqual(code, EXIT_HOST_GONE)
        self.assertTrue(any("host is gone" in line for line in logs.output))


class ProcessTests(unittest.TestCase):
    """``server.py`` as the host runs it: a child process on pipes."""

    def test_fake_server_end_to_end(self) -> None:
        """Requests, a bad one and a failing one, then EOF: exit 0."""
        stdin = (
            detect_bytes(detect_header(1))
            + detect_bytes(detect_header(2), payload=b"")
            + detect_bytes(detect_header(3, phrases=[{"text": FAIL_PHRASE, "threshold": 0.5}]))
            + detect_bytes(detect_header(4, width=1280, height=720))
        )
        code, frames, stderr = run_server(["--backend", "fake", "--log-level", "debug"], stdin)
        self.assertEqual(code, EXIT_OK, stderr)
        self.assertEqual(
            [(f.header["type"], f.header.get("id")) for f in frames],
            [("hello", None), ("result", 1), ("error", 2), ("error", 3), ("result", 4)],
        )
        self.assertIn("stdin closed", stderr)
        self.assertIn("request 4", stderr)

    def test_bad_command_line_is_a_fatal_frame(self) -> None:
        """Invalid arguments are reported on the protocol, exit code 2."""
        for args in (
            ["--side", "100"],
            ["--side", "1000"],
            ["--backend", "mlx", "--model", "/nonexistent/sam31-mlx", "--side", "686"],
            ["--backend", "mlx"],
            ["--backend", "torch"],
            ["--nope"],
        ):
            with self.subTest(args=args):
                code, frames, _ = run_server(args, b"")
                self.assertEqual(code, 2)
                self.assertEqual(len(frames), 1)
                self.assertEqual(frames[0].header["type"], "error")
                self.assertIsNone(frames[0].header["id"])
                self.assertIn("bad command line", frames[0].header["message"])

    def test_a_side_sam_cannot_run_at_is_a_usage_error(self) -> None:
        """``--side`` must be a multiple of 28; checked before any model loads."""
        import server

        for side in (1000, 686, 223, 1036):
            with self.subTest(side=side):
                with self.assertRaises(server.UsageError) as caught:
                    server.parse_args(["--backend", "mlx", "--model", "m", "--side", str(side)])
                self.assertIn("multiple of 28", str(caught.exception))
        for side in (224, 672, 700, 1008):
            args = server.parse_args(["--backend", "fake", "--side", str(side)])
            self.assertEqual(args.side, side)

    def test_the_watchdog_exits_once_the_parent_is_gone(self) -> None:
        """A parent id that is not ours any more makes the watchdog exit with 5."""
        import server

        exits: list[int] = []
        with self.assertLogs("sam3_sidecar", "ERROR"):
            thread = server.watch_parent(os.getppid() + 1_000_000, exits.append, poll_s=0.01)
            thread.join(timeout=5.0)
        self.assertFalse(thread.is_alive())
        self.assertEqual(exits, [EXIT_HOST_GONE])

    def test_the_server_does_not_outlive_its_host(self) -> None:
        """A host that dies without closing stdin leaves no server behind.

        ``sh`` starts the server in the background, prints its pid and exits,
        while this test keeps the server's stdin open: only the watchdog can
        end it.
        """
        read_end, write_end = os.pipe()
        try:
            command = f"{shlex.quote(sys.executable)} {shlex.quote(str(SERVER))} --backend fake"
            proc = subprocess.run(
                ["sh", "-c", f"{command} >/dev/null 2>&1 & echo $!"],
                stdin=read_end,
                capture_output=True,
                timeout=CHILD_TIMEOUT_S,
                check=True,
            )
            pid = int(proc.stdout.strip())
            deadline = time.monotonic() + 10.0
            while _running(pid) and time.monotonic() < deadline:
                time.sleep(0.05)
            alive = _running(pid)
            if alive:
                os.kill(pid, signal.SIGKILL)
            self.assertFalse(alive, "the server outlived its host")
        finally:
            os.close(read_end)
            os.close(write_end)

    def test_missing_model_folder_is_a_fatal_load_error(self) -> None:
        """The MLX backend checks its folder before importing MLX."""
        code, frames, _ = run_server(["--backend", "mlx", "--model", "/nonexistent/sam31-mlx"], b"")
        self.assertEqual(code, EXIT_LOAD_FAILED)
        self.assertEqual(len(frames), 1)
        self.assertIsNone(frames[0].header["id"])
        self.assertIn("does not exist", frames[0].header["message"])

    def test_garbage_input_is_fatal(self) -> None:
        """A corrupt length prefix ends the child with a fatal error."""
        code, frames, _ = run_server(["--backend", "fake"], b"\xff\xff\xff\xff")
        self.assertEqual(code, EXIT_PROTOCOL)
        self.assertEqual([f.header["type"] for f in frames], ["hello", "error"])

    def test_runs_in_isolated_mode(self) -> None:
        """``python -I server.py`` still finds its package."""
        code, frames, stderr = run_server(["--backend", "fake"], b"", python_flags=("-I",))
        self.assertEqual(code, EXIT_OK, stderr)
        self.assertEqual([f.header["type"] for f in frames], ["hello"])

    def test_stray_output_goes_to_stderr(self) -> None:
        """After ``claim_stdio`` print and raw fd 1 writes reach stderr only."""
        code, out, err = run_python(
            "import os, server\n"
            "reader, writer = server.claim_stdio()\n"
            "print('python noise')\n"
            "os.write(1, b'native noise\\n')\n"
            "writer.write(b'FRAME')\n"
            "writer.flush()\n"
            "print('protocol in', repr(reader.read()))\n"
            "print('fd 0 now', repr(os.read(0, 16)))\n",
            stdin=b"from host",
        )
        self.assertEqual(code, 0, err)
        self.assertEqual(out, b"FRAME")
        self.assertIn("python noise", err)
        self.assertIn("native noise", err)
        self.assertIn("protocol in b'from host'", err)
        self.assertIn("fd 0 now b''", err)


def _running(pid: int) -> bool:
    """Tell whether ``pid`` exists and is not a zombie (no init may reap it)."""
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    try:
        with open(f"/proc/{pid}/stat", encoding="ascii") as stat:
            return stat.read().rsplit(")", 1)[1].split()[0] != "Z"
    except OSError:
        ps = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True)
        return ps.returncode == 0 and not ps.stdout.strip().startswith("Z")


if __name__ == "__main__":
    unittest.main()
