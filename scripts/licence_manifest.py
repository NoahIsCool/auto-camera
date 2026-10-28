#!/usr/bin/env python3
"""Fails when a part of Auto Camera that is not a crate has no licence entry.

cargo-deny (`deny.toml`) checks the licence of every crate. It cannot see
the parts the app runs, links or loads that do not come from crates.io: the
ffmpeg programs, the ONNX Runtime library that `ort` links in, model
weights and the detector sidecar's Python packages. Their licences are in
`docs/licence-manifest.md`, and this check keeps that file complete: it
finds each such component from what the repository itself declares, and
every one must have a valid row in the manifest's table.

A component is found, and named by an id, by these rules:

* `pypi:<name>`: each requirement of a `sidecars/*/requirements.txt`, and
  of the files it includes with `-r`, named as pip compares names (lower
  case, each run of `-`, `_` and `.` as one `-`). A requirement with no
  project name, a bare URL or path, is a problem: it cannot be named.
* `model:<folder>`: each model path in the `[detector]` table of a studio
  config, `config/*.toml` or `config/*.json` (`model`, `text_encoder`,
  `tokenizer`, and the argument of `--model` in `command`), named by the
  folder it is or is in: a last path component with an extension is a
  file, and its folder is its parent.
* `native:onnxruntime`: a dependency on the `ort` crate, under any name, in
  a crate manifest under `crates/` or `apps/` (ort links ONNX Runtime into
  the binary).
* `native:ffmpeg`: the string literal `"ffmpeg"` or `"ffprobe"` in a Rust
  source under `crates/*/src` or `apps/*/src` (the tools are run by name).

The first two rules find new components by themselves. The last two cover
the only external program and native library the app uses today; a new one
needs a rule here.

The manifest's table is the one whose header row starts with `Id`, and its
columns are exactly `COLUMNS` (cells may not contain `|`). A row is valid
when its Id cell holds one or more ids in backquotes, each a kind (`pypi`,
`model` or `native`), a colon and a name; no id is in two rows; its Licence
cell is not empty; its Licence source cell has an `https://` link; and its
Checked cell is `yes`, `partly` or `no`. A row whose ids no rule finds is
reported as unused but does not fail the check, so a licence entry can
outlive the example config that named it.

Usage: `python3 scripts/licence_manifest.py [ROOT]`. Exits 0 when every
component has a valid row, 1 when one has none or the manifest is invalid
(each problem is listed), 2 on bad usage. Standard library only; Python
3.11 or newer (`tomllib`).
"""

import argparse
import json
import re
import sys
import tomllib
from pathlib import Path, PurePosixPath

#: The manifest, relative to the repository root.
MANIFEST = PurePosixPath("docs/licence-manifest.md")
#: The columns of the manifest's table, in order.
COLUMNS = ("Id", "Component", "Licence", "Licence source", "Checked")
#: The values the Checked column may hold.
CHECKED = ("yes", "partly", "no")
#: Keys of a studio config's `[detector]` table that hold model paths.
MODEL_KEYS = ("model", "text_encoder", "tokenizer")
#: Dependency tables of a crate manifest, at the top or under a target.
DEPENDENCY_TABLES = ("dependencies", "dev-dependencies", "build-dependencies")
#: The project name at the start of a requirement line (PEP 508): a name
#: followed by the end, extras, a version, a marker or `@ url`. A bare URL
#: or path has none.
REQUIREMENT_NAME = re.compile(
    r"[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?(?=\s*(?:$|[\[(<>=!~;@,]))"
)
#: A `-r FILE` / `--requirement FILE` include; group 1 is the file.
INCLUDE = re.compile(r"(?:-r|--requirement)(?:\s+|=)(\S+)")
#: A pip comment: `#` at the start of a line or after white space.
COMMENT = re.compile(r"(?:^|\s)#.*")
#: String literals naming the ffmpeg tools in Rust source.
FFMPEG_LITERAL = re.compile(r'"(?:ffmpeg|ffprobe)"')
#: An id in backquotes in the Id column; group 1 is the id.
ID_IN_CELL = re.compile(r"`([^`]*)`")
#: A well-formed id: a kind, a colon and a name without spaces or colons.
ID_FORM = re.compile(r"(?:pypi|model|native):[^\s:`]+")


def normalize(name):
    """`name` as pip compares project names: lower case, with each run of
    `-`, `_` and `.` as one `-` (PEP 503)."""
    return re.sub(r"[-_.]+", "-", name).lower()


def shown(path, root):
    """`path` as messages name it: relative to `root` when inside it."""
    return path.relative_to(root) if path.is_relative_to(root) else path


def requirement_names(path, root, problems, seen=None):
    """The project names that the requirements file `path` lists, and those
    of the files it includes with `-r`, as `(name, file)` pairs. Comments,
    blank lines and other options are skipped. An include that does not
    exist, and a requirement without a project name (a bare URL or path),
    go to `problems`, the file named relative to `root`. `seen` stops
    include cycles."""
    seen = set() if seen is None else seen
    if path in seen:
        return []
    seen.add(path)
    found = []
    for number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = COMMENT.sub("", raw).strip()
        where = f"{shown(path, root)}:{number}"
        if not line:
            continue
        if line.startswith("-"):
            if include := INCLUDE.fullmatch(line):
                target = path.parent / include.group(1)
                if target.is_file():
                    found += requirement_names(target, root, problems, seen)
                else:
                    problems.append(f"{where}: includes {include.group(1)}, which does not exist")
        elif name := REQUIREMENT_NAME.match(line):
            found.append((normalize(name.group(0)), path))
        else:
            problems.append(f"{where}: no project name in {line!r}; write it as `name @ url`")
    return found


def model_folder(path):
    """The folder a model path names: the path itself, or the parent of a
    file (a last component with an extension), or the file's own name when
    it has no folder."""
    p = PurePosixPath(path)
    if not p.suffix:
        return p.name
    return p.parent.name or p.name


def detector_model_paths(config):
    """The model paths in the `[detector]` table of the parsed studio
    config `config`: its `MODEL_KEYS`, and the argument of `--model` (or
    `--model=PATH`) in its `command`."""
    detector = config.get("detector")
    if not isinstance(detector, dict):
        return []
    paths = [detector[k] for k in MODEL_KEYS if isinstance(detector.get(k), str)]
    command = detector.get("command")
    if isinstance(command, list):
        for i, arg in enumerate(command):
            if not isinstance(arg, str):
                continue
            if arg == "--model" and i + 1 < len(command) and isinstance(command[i + 1], str):
                paths.append(command[i + 1])
            elif arg.startswith("--model="):
                paths.append(arg.removeprefix("--model="))
    return [p for p in paths if p]


def depends_on_ort(manifest):
    """True when the parsed crate manifest `manifest` depends on the `ort`
    crate in any dependency table, target-specific ones included, under
    its own name or renamed (`package = "ort"`)."""
    tables = [manifest.get(t, {}) for t in DEPENDENCY_TABLES]
    for target in manifest.get("target", {}).values():
        tables += [target.get(t, {}) for t in DEPENDENCY_TABLES]
    for table in tables:
        for key, spec in table.items():
            name = spec.get("package", key) if isinstance(spec, dict) else key
            if name == "ort":
                return True
    return False


def load_config(path):
    """The studio config at `path`, parsed as TOML or, for `.json`, JSON."""
    if path.suffix == ".json":
        return json.loads(path.read_text(encoding="utf-8"))
    return tomllib.loads(path.read_text(encoding="utf-8"))


def components(root, problems):
    """Every component the rules (see the module docs) find under `root`,
    as a dict from id to the sorted files, relative to `root`, that show
    it. Files that cannot be parsed go to `problems`."""
    found = {}

    def add(component, path):
        """Records that `path` shows `component`."""
        found.setdefault(component, set()).add(str(shown(path, root)))

    for requirements in sorted(root.glob("sidecars/*/requirements.txt")):
        for name, path in requirement_names(requirements, root, problems):
            add(f"pypi:{name}", path)
    configs = sorted([*root.glob("config/*.toml"), *root.glob("config/*.json")])
    for path in configs:
        try:
            config = load_config(path)
        except (tomllib.TOMLDecodeError, json.JSONDecodeError) as e:
            problems.append(f"{path.relative_to(root)}: cannot be parsed: {e}")
            continue
        for model in detector_model_paths(config):
            add(f"model:{model_folder(model)}", path)
    for path in sorted([*root.glob("crates/*/Cargo.toml"), *root.glob("apps/*/Cargo.toml")]):
        try:
            manifest = tomllib.loads(path.read_text(encoding="utf-8"))
        except tomllib.TOMLDecodeError as e:
            problems.append(f"{path.relative_to(root)}: cannot be parsed: {e}")
            continue
        if depends_on_ort(manifest):
            add("native:onnxruntime", path)
    for path in sorted([*root.glob("crates/*/src/**/*.rs"), *root.glob("apps/*/src/**/*.rs")]):
        if FFMPEG_LITERAL.search(path.read_text(encoding="utf-8", errors="replace")):
            add("native:ffmpeg", path)
    return {component: sorted(paths) for component, paths in found.items()}


def table_rows(text, problems):
    """The rows of the manifest's table in `text`, as `(line number, cells)`
    pairs. Problems with the table itself (none found, wrong columns, a row
    with the wrong number of cells) go to `problems`."""
    rows, header = [], None
    for number, raw in enumerate(text.splitlines(), 1):
        line = raw.strip()
        if not line.startswith("|"):
            if header is not None:
                break
            continue
        cells = [c.strip() for c in line.strip("|").split("|")]
        if header is None:
            if cells[0] == "Id":
                header = cells
                if tuple(cells) != COLUMNS:
                    problems.append(
                        f"{MANIFEST}:{number}: the table's columns must be {' | '.join(COLUMNS)}"
                    )
                    return []
            continue
        if all(c and set(c) <= set("-: ") for c in cells):
            continue
        if len(cells) != len(COLUMNS):
            problems.append(f"{MANIFEST}:{number}: {len(cells)} cells, not {len(COLUMNS)}")
            continue
        rows.append((number, cells))
    if header is None:
        problems.append(f"{MANIFEST}: no table whose header row starts with Id")
    return rows


def manifest_ids(text, problems):
    """The ids of the valid rows of the manifest `text`, as a dict from id
    to its row's line number. Each invalid row, and each id in two rows,
    goes to `problems` (see the module docs for what makes a row valid)."""
    listed = {}
    for number, cells in table_rows(text, problems):
        ids_cell, _component, licence, source, checked = cells
        where = f"{MANIFEST}:{number}"
        ids = ID_IN_CELL.findall(ids_cell)
        errors = []
        if not ids:
            errors.append("no id in backquotes in the Id column")
        errors += [
            f"`{i}` is not kind:name (kind pypi, model or native)"
            for i in ids
            if not ID_FORM.fullmatch(i)
        ]
        if not licence:
            errors.append("the Licence column is empty")
        if "https://" not in source:
            errors.append("the Licence source column has no https:// link")
        if checked not in CHECKED:
            errors.append(f"Checked is {checked!r}, not one of {', '.join(CHECKED)}")
        if errors:
            problems += [f"{where}: {e}" for e in errors]
            continue
        for i in ids:
            if i in listed:
                problems.append(f"{where}: `{i}` is already in the row on line {listed[i]}")
            else:
                listed[i] = number
    return listed


def check(root):
    """Checks the repository at `root`. Returns `(found, missing, problems,
    unused)`: the ids of every component the rules find, sorted; those of
    them with no valid row, as `(id, files that show it)`; the problems
    with the manifest or the files read, as messages; and the ids of valid
    rows that no rule finds, sorted."""
    root = root.resolve()
    problems = []
    found = components(root, problems)
    try:
        text = (root / MANIFEST).read_text(encoding="utf-8")
    except FileNotFoundError:
        problems.append(f"{MANIFEST}: missing")
        listed = {}
    else:
        listed = manifest_ids(text, problems)
    missing = [(c, found[c]) for c in sorted(found) if c not in listed]
    unused = sorted(c for c in listed if c not in found)
    return sorted(found), missing, problems, unused


def main(argv=None):
    """Runs the check from the command line; returns the exit status."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "root",
        nargs="?",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="repository root (default: the one holding this script)",
    )
    args = parser.parse_args(argv)
    if not args.root.is_dir():
        parser.error(f"{args.root} is not a directory")
    found, missing, problems, unused = check(args.root)
    for component, files in missing:
        print(f"{component} has no licence entry in {MANIFEST} (found in {', '.join(files)})")
    for problem in problems:
        print(problem)
    for component in unused:
        print(f"note: {component} has a licence entry, but nothing in the repository uses it")
    if missing or problems:
        print(
            f"Give each component a row in {MANIFEST}: its licence, where the licence "
            "text is, and whether it was checked there (the file says how)."
        )
        return 1
    print(f"{len(found)} non-crate components, each with a licence entry: {', '.join(found)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
