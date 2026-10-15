r"""Checks and fingerprints for an MLX SAM 3.1 weights folder.

Theory
------
The weights folder (Noah's ``models/sam31-mlx``) holds ``config.json``, one
or more ``*.safetensors`` files and a ``tokenizer/`` subfolder for the CLIP
tokenizer. Checking those before importing MLX turns a wrong path into a
clear fatal error in milliseconds instead of a stack trace seconds later, and
works on machines without MLX.

``weight_files`` picks the files exactly as mlx-vlm's ``load_model`` does at
the pinned revision (``mlx_vlm/utils.py``, revision
5b22c24f325a953ec70b965e39fb97a9b5ec7a4d): the shards named by
``model.safetensors.index.json`` that exist, else every ``*.safetensors``
except ``consolidated.safetensors``. ``weights_sha256`` fingerprints those
files so the host can tell which weights answered:

* one file: its SHA-256, the same as ``shasum -a 256 <file>``;
* several: the SHA-256 of the manifest of ``"<hex>  <name>\\n"`` lines in
  name order, the same as ``LC_ALL=C shasum -a 256 <names in order> |
  shasum -a 256`` run inside the folder.

Invariants
----------
* Stdlib only; files are read in fixed-size chunks, never whole.
* The fingerprint depends only on file names and bytes, not on paths or
  timestamps, so a moved or copied folder keeps its fingerprint.
"""

from __future__ import annotations

import glob
import hashlib
import json
from pathlib import Path

#: Read size used while hashing weights.
_CHUNK = 8 << 20


class ModelFolderError(Exception):
    """The weights folder is missing something the MLX backend needs."""


def weight_files(folder: Path) -> list[Path]:
    """List the safetensors files mlx-vlm's ``load_model`` would load, by name."""
    index = folder / "model.safetensors.index.json"
    if index.exists():
        try:
            with open(index, encoding="utf-8") as handle:
                weight_map = json.load(handle).get("weight_map", {})
            shards = sorted({str(name) for name in weight_map.values()})
            found = [folder / name for name in shards if (folder / name).exists()]
        except (ValueError, OSError, AttributeError):
            found = []
        if found:
            return found
    names = glob.glob(str(folder / "*.safetensors"))
    return sorted(
        (Path(name) for name in names if not name.endswith("consolidated.safetensors")),
        key=lambda path: path.name,
    )


def check_model_folder(folder: Path) -> None:
    """Raise ``ModelFolderError`` unless ``folder`` looks like the weights."""
    if not folder.is_dir():
        raise ModelFolderError(f"model folder {folder} does not exist or is not a folder")
    if not (folder / "config.json").is_file():
        raise ModelFolderError(f"model folder {folder} has no config.json")
    if not (folder / "tokenizer").is_dir():
        raise ModelFolderError(f"model folder {folder} has no tokenizer/ subfolder")
    if not weight_files(folder):
        raise ModelFolderError(f"model folder {folder} has no .safetensors weights")


def _file_sha256(path: Path) -> str:
    """Hex SHA-256 of one file, read in chunks."""
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(_CHUNK), b""):
            digest.update(chunk)
    return digest.hexdigest()


def weights_sha256(folder: Path) -> str:
    """Fingerprint the weight files (see the module docs for the rule)."""
    files = weight_files(folder)
    if not files:
        raise ModelFolderError(f"model folder {folder} has no .safetensors weights")
    if len(files) == 1:
        return _file_sha256(files[0])
    manifest = "".join(
        f"{_file_sha256(path)}  {path.relative_to(folder).as_posix()}\n" for path in files
    )
    return hashlib.sha256(manifest.encode("utf-8")).hexdigest()
