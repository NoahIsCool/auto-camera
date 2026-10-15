"""Shared helpers: build host frames, parse child output, run the server.

Every child process started here is waited for or killed before the helper
returns, so no test leaves a process behind.
"""

from __future__ import annotations

import io
import os
import subprocess
import sys
from pathlib import Path
from typing import Any

from sam3_sidecar.protocol import Frame, encode_frame, read_frame

#: The sidecar's entry point.
SERVER = Path(__file__).resolve().parent.parent / "server.py"

#: Longest a test lets one child run, in seconds.
CHILD_TIMEOUT_S = 60.0


def detect_header(request_id: Any = 1, **overrides: Any) -> dict[str, Any]:
    """Build a valid 4x2 ``detect`` header for ``person``, then apply overrides."""
    header: dict[str, Any] = {
        "type": "detect",
        "id": request_id,
        "source": "cam-1",
        "seq": 7,
        "pts_ns": 1_000_000,
        "width": 4,
        "height": 2,
        "format": "rgb8",
        "phrases": [{"text": "person", "threshold": 0.5}],
        "want_masks": False,
    }
    header.update(overrides)
    return header


def pixels_for(header: dict[str, Any]) -> bytes:
    """Grey RGB8 pixels of exactly the header's size."""
    return bytes([128]) * (int(header["width"]) * int(header["height"]) * 3)


def detect_bytes(header: dict[str, Any], payload: bytes | None = None) -> bytes:
    """Encode a ``detect`` frame; the payload defaults to matching pixels."""
    return encode_frame(header, pixels_for(header) if payload is None else payload)


def parse_frames(data: bytes) -> list[Frame]:
    """Decode every frame in ``data``; fail if any bytes are left over."""
    stream = io.BytesIO(data)
    frames: list[Frame] = []
    while True:
        frame = read_frame(stream)
        if frame is None:
            return frames
        frames.append(frame)


def run_server(
    args: list[str],
    stdin: bytes,
    *,
    python_flags: tuple[str, ...] = (),
    timeout: float = CHILD_TIMEOUT_S,
) -> tuple[int, list[Frame], str]:
    """Run ``server.py`` to completion; return exit code, frames and stderr.

    The child is killed if it outlives ``timeout`` seconds.
    """
    command = [sys.executable, *python_flags, str(SERVER), *args]
    return _run(command, stdin, timeout)


def run_python(code: str, stdin: bytes = b"") -> tuple[int, bytes, str]:
    """Run ``python -c code`` in the sidecar folder; return code, stdout, stderr."""
    proc = subprocess.Popen(
        [sys.executable, "-c", code],
        cwd=str(SERVER.parent),
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=_child_env(),
    )
    try:
        out, err = proc.communicate(stdin, timeout=CHILD_TIMEOUT_S)
    finally:
        _reap(proc)
    return proc.returncode, out, err.decode("utf-8", "replace")


def _run(command: list[str], stdin: bytes, timeout: float) -> tuple[int, list[Frame], str]:
    """Run ``command`` with pipes and parse its stdout as frames."""
    proc = subprocess.Popen(
        command,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=_child_env(),
    )
    try:
        out, err = proc.communicate(stdin, timeout=timeout)
    finally:
        _reap(proc)
    return proc.returncode, parse_frames(out), err.decode("utf-8", "replace")


def _child_env() -> dict[str, str]:
    """Copy the test's environment, without bytecode files in the source tree."""
    env = dict(os.environ)
    env["PYTHONDONTWRITEBYTECODE"] = "1"
    return env


def _reap(proc: subprocess.Popen[bytes]) -> None:
    """Kill ``proc`` if it is still running, then wait for it."""
    if proc.poll() is None:
        proc.kill()
    proc.wait()
