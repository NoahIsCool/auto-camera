#!/usr/bin/env python3
"""Entry point of the SAM 3.1 sidecar: parse arguments, claim stdio, serve.

Theory
------
The host reads protocol frames from this process's stdout, so a single stray
byte there (a library's ``print``, a progress bar, a native library writing
to fd 1) would desynchronize the stream. ``claim_stdio`` therefore takes the
protocol ends first, before any other module can write: it duplicates fd 0
and fd 1 into private streams for the protocol, points fd 1 at stderr and
fd 0 at ``/dev/null``, and makes ``sys.stdout`` an alias of ``sys.stderr``.
After that, everything that is not a protocol frame lands in the log.

Usage::

    python server.py --backend mlx --model PATH [--side 672] [--log-level info]
    python server.py --backend fake [--side 672]

Invariants
----------
* stdout carries protocol frames only; logs and everything else go to stderr.
* A bad command line is reported as a fatal error frame (exit code 2), so the
  host sees why the child refused to start. That includes a ``--side`` SAM 3.1
  cannot run at (not a multiple of 28 in 224..1008), checked here in
  milliseconds rather than after hashing and loading gigabytes of weights.
* The process never outlives the host: a watchdog thread exits with
  ``EXIT_HOST_GONE`` (5) as soon as the parent process is gone (the child is
  re-parented, so ``os.getppid()`` changes). That covers a host killed
  without closing stdin (SIGKILL, a crash, a closed terminal) while this
  process is busy loading or answering, on macOS too, which has no
  parent-death signal. The check runs between Python bytecodes, so a native
  call that holds the GIL delays it until the call returns.
* The process exits on stdin EOF; see ``sam3_sidecar.loop`` for exit codes.
* This file and the package import only the standard library; the MLX
  backend imports MLX itself when it loads.
"""

from __future__ import annotations

import argparse
import io
import logging
import os
import sys
import threading
import time
from collections.abc import Callable
from pathlib import Path
from typing import NoReturn

#: Folder holding this script and the ``sam3_sidecar`` package. Added to
#: ``sys.path`` explicitly so the package imports even under ``python -I``.
_HERE = Path(__file__).resolve().parent
if str(_HERE) not in sys.path:
    sys.path.insert(0, str(_HERE))

from sam3_sidecar.backend import MAX_SIDE, MIN_SIDE, SAM31_PATCH, Backend, check_side  # noqa: E402
from sam3_sidecar.fake import FakeBackend  # noqa: E402
from sam3_sidecar.loop import EXIT_HOST_GONE, send_fatal, serve  # noqa: E402

#: Exit code for a bad command line (argparse's own convention).
EXIT_USAGE = 2

#: Exit code after Ctrl+C when run by hand.
EXIT_INTERRUPTED = 130

#: Square side the model runs at unless ``--side`` says otherwise.
DEFAULT_SIDE = 672

#: Names accepted by ``--backend``.
BACKENDS = ("mlx", "fake")

#: Names accepted by ``--log-level``.
LOG_LEVELS = ("debug", "info", "warning", "error")

#: How often the watchdog checks that the parent is still there, in seconds.
PARENT_POLL_S = 0.25


class UsageError(Exception):
    """The command line is invalid; the message says how."""


class _Parser(argparse.ArgumentParser):
    """An argument parser that raises instead of printing and exiting."""

    def error(self, message: str) -> NoReturn:
        """Raise ``UsageError`` so ``main`` can send a fatal error frame."""
        raise UsageError(message)


def build_parser() -> argparse.ArgumentParser:
    """Build the command-line parser of the sidecar."""
    parser = _Parser(
        prog="server.py",
        description="SAM 3.1 sidecar for Auto Camera (protocol v1 on stdin/stdout).",
    )
    parser.add_argument("--model", type=Path, help="MLX SAM 3.1 weights folder (mlx backend)")
    parser.add_argument(
        "--side",
        type=int,
        default=DEFAULT_SIDE,
        help=(
            f"square side the model runs at, a multiple of {2 * SAM31_PATCH} in "
            f"{MIN_SIDE}..{MAX_SIDE} (default {DEFAULT_SIDE})"
        ),
    )
    parser.add_argument("--backend", choices=BACKENDS, default="mlx", help="default mlx")
    parser.add_argument(
        "--log-level",
        type=str.lower,
        choices=LOG_LEVELS,
        default="info",
        help="stderr log level (default info)",
    )
    return parser


def parse_args(argv: list[str] | None) -> argparse.Namespace:
    """Parse and cross-check arguments; raise ``UsageError`` when invalid."""
    args = build_parser().parse_args(argv)
    try:
        check_side(args.side, SAM31_PATCH)
    except ValueError as exc:
        raise UsageError(f"--side {args.side}: {exc}") from exc
    if args.backend == "mlx" and args.model is None:
        raise UsageError("--model is required with --backend mlx")
    return args


def claim_stdio() -> tuple[io.BufferedReader, io.BufferedWriter]:
    """Take fd 0 and fd 1 for the protocol; route all other output to stderr.

    Returns ``(reader, writer)`` for the protocol. Afterwards fd 1 is a copy
    of stderr, fd 0 reads ``/dev/null`` and ``sys.stdout`` is ``sys.stderr``.
    """
    sys.stdout.flush()
    reader = os.fdopen(os.dup(0), "rb")
    writer = os.fdopen(os.dup(1), "wb")
    os.dup2(2, 1)
    null = os.open(os.devnull, os.O_RDONLY)
    try:
        os.dup2(null, 0)
    finally:
        os.close(null)
    sys.stdout = sys.stderr
    return reader, writer


def configure_logging(level: str) -> None:
    """Send every log record at ``level`` or above to stderr."""
    logging.basicConfig(
        stream=sys.stderr,
        level=getattr(logging, level.upper()),
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
        force=True,
    )
    logging.captureWarnings(True)


def make_loader(args: argparse.Namespace) -> Callable[[], Backend]:
    """Return a callable that builds the backend ``--backend`` names."""
    if args.backend == "fake":
        if args.model is not None:
            logging.getLogger("sam3_sidecar").info("fake backend ignores --model %s", args.model)
        return lambda: FakeBackend(input_side=args.side)

    def load_mlx() -> Backend:
        """Import the MLX backend only now, so the fake never needs it."""
        from sam3_sidecar.mlx_sam31 import Sam31Backend

        return Sam31Backend.load(args.model.expanduser(), args.side)

    return load_mlx


def watch_parent(
    parent: int,
    exit_now: Callable[[int], object] = os._exit,
    poll_s: float = PARENT_POLL_S,
) -> threading.Thread:
    """Start a daemon thread that exits the process once ``parent`` is gone.

    ``exit_now`` ends the process with ``EXIT_HOST_GONE`` (``os._exit``: no
    cleanup, which could block on the dead host's pipes). Returns the thread.
    """

    def watch() -> None:
        """Poll the parent id until it changes."""
        while os.getppid() == parent:
            time.sleep(poll_s)
        try:
            # stderr is a pipe to the dead host: the log may fail.
            logging.getLogger("sam3_sidecar").error("the host (pid %d) is gone; exiting", parent)
        finally:
            exit_now(EXIT_HOST_GONE)

    thread = threading.Thread(target=watch, name="parent-watchdog", daemon=True)
    thread.start()
    return thread


def main(argv: list[str] | None = None) -> int:
    """Run the sidecar and return its exit code."""
    watch_parent(os.getppid())
    reader, writer = claim_stdio()
    try:
        args = parse_args(argv)
    except UsageError as exc:
        configure_logging("info")
        send_fatal(writer, f"bad command line: {exc}")
        return EXIT_USAGE
    configure_logging(args.log_level)
    try:
        return serve(reader, writer, make_loader(args))
    except KeyboardInterrupt:
        return EXIT_INTERRUPTED


if __name__ == "__main__":
    sys.exit(main())
