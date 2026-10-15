"""SAM 3.1 sidecar for Auto Camera: protocol v1 over stdin and stdout.

The Rust host (``crates/ac-sidecar``) spawns ``server.py`` with stdin and
stdout as pipes, waits for ``hello`` and then sends one ``detect`` at a time.
This package holds everything behind that entry point:

* ``protocol``: framing, validation and message builders (stdlib only);
* ``backend``: the ``Backend`` interface and its pure geometry helpers;
* ``fake``: deterministic boxes for tests (stdlib only);
* ``mlx_sam31``: SAM 3.1 through mlx-vlm on Apple Silicon (MLX imported
  lazily);
* ``model_files``: weights-folder checks and the weights fingerprint;
* ``loop``: the request loop and exit codes.

Invariants shared by every module: stdout carries protocol frames only, logs
go to stderr, and nothing here imports a third-party package at import time.
"""
