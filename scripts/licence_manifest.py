#!/usr/bin/env python3
"""Fails when a part of Auto Camera that is not a crate has no licence entry.

cargo-deny (`deny.toml`) checks the licence of every crate. It cannot see
the parts the app runs, links or loads that do not come from crates.io: the
ffmpeg programs, macOS's `caffeinate`, the ONNX Runtime library that `ort`
links in, model weights, and the detector sidecar's interpreter and Python
packages. Their licences are in `docs/licence-manifest.md`, and this check
keeps that file complete: it finds each such component from what the
repository itself declares, and every one must have a valid row in the
manifest's table.

A component is found, and named by an id, by these rules:

* `pypi:<name>`: each project pip would install from a
  `sidecars/*/requirements.txt`, named as pip compares names (lower case,
  each run of `-`, `_` and `.` as one `-`). The file is read as pip 24
  reads it (see `requirement_names`): its requirements and editables
  (`-e`), those of the files it includes (`-r`), and those that a
  constraints file (`-c`) includes, since pip only limits the versions of
  what a constraints file names. A project the check cannot name is a
  problem: a bare URL or path (write `name @ url`), an editable with no
  `#egg=` name, and a line whose options pip would reject or ignore.
* `model:<folder>`: each model path in the `[detector]` table of a studio
  config, `config/*.toml` or `config/*.json` (`model`, `text_encoder`,
  `tokenizer`, and the argument of `--model` in `command`), named by the
  folder it is or is in: a last path component with an extension is a
  file, and its folder is its parent.
* `native:<program>`: the program the `[detector]` table of a studio
  config runs (the first element of its `command`), named by its file
  name; a Python interpreter (`python`, `python3`, `python3.13`) is
  `native:python`.
* `native:onnxruntime`: a dependency on the `ort` crate, under any name, in
  a crate manifest under `crates/` or `apps/` (ort links ONNX Runtime into
  the binary).
* `native:ffmpeg`: the string literal `"ffmpeg"` or `"ffprobe"` in a Rust
  source under `crates/*/src` or `apps/*/src` (the tools are run by name).
* `native:caffeinate`: the string literal `"caffeinate"` or
  `"/usr/bin/caffeinate"` in a Rust source under `crates/*/src` or
  `apps/*/src` (macOS's tool, which keeps the system awake, is run by its
  path).

The first three rules find new components by themselves: every project
pip would install from a sidecar's requirements is found or reported, and
the check may find more than pip installs (pip reads only the first
requirement, editable or include of a line, the check all of them). The
last three cover the one native library the app links and the two
programs it runs under a fixed name today (`FIXED_PROGRAMS`); a new one
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
import optparse
import re
import shlex
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
#: The options pip 24 takes in a requirements file
#: (`pip._internal.req.req_file`): the spellings of each, and whether it
#: takes a value. Only `-r`, `-c` and `-e` read or install anything; pip
#: rejects a line with any other option.
PIP_OPTIONS = (
    (("-i", "--index-url", "--pypi-url"), True),
    (("--extra-index-url",), True),
    (("--no-index",), False),
    (("-c", "--constraint"), True),
    (("-r", "--requirement"), True),
    (("-e", "--editable"), True),
    (("-f", "--find-links"), True),
    (("--no-binary",), True),
    (("--only-binary",), True),
    (("--prefer-binary",), False),
    (("--require-hashes",), False),
    (("--pre",), False),
    (("--trusted-host",), True),
    (("--use-feature",), True),
    (("--global-option",), True),
    (("--hash",), True),
    (("-C", "--config-settings"), True),
)
#: A pip comment: `#` at the start of a line or after white space.
COMMENT = re.compile(r"(?:^|\s+)#.*")
#: The `#egg=` name of an editable; group 1 is the name and any extras.
EGG = re.compile(r"[#&]egg=([^&]*)")
#: An include that pip downloads instead of reading a file.
URL = re.compile(r"(?:http|https|file):", re.IGNORECASE)
#: The file name of a Python interpreter: `python`, `python3`, `python3.13`.
PYTHON = re.compile(r"python[0-9.]*")
#: The programs the app runs under a fixed name: each id, and the string
#: literals in Rust source that name the program.
FIXED_PROGRAMS = (
    ("native:ffmpeg", re.compile(r'"(?:ffmpeg|ffprobe)"')),
    ("native:caffeinate", re.compile(r'"(?:/usr/bin/)?caffeinate"')),
)
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


class OptionError(Exception):
    """An option of a requirements line that pip would reject."""


class LineOptions(optparse.OptionParser):
    """pip's parser for the options of one requirements line: the options
    of `PIP_OPTIONS`, spelled as optparse allows (`-rFILE`, `-r FILE`,
    `--requirement=FILE`, and a long option cut short where that is
    unambiguous, as `--edit URL`), each value option collecting its values
    in order. Raises `OptionError` where optparse would exit."""

    def __init__(self):
        super().__init__(add_help_option=False)
        for flags, takes_value in PIP_OPTIONS:
            self.add_option(*flags, action="append" if takes_value else "store_true")

    def error(self, msg):
        """Raises `OptionError` with optparse's message instead of exiting."""
        raise OptionError(msg)


def logical_lines(text):
    """The lines of the requirements file `text` as pip reads them, as
    `(line number, line)` pairs: a line ending in `\\` joined to the next
    (except a comment line, which ends the join), then comments removed,
    white space stripped and blank lines dropped. A joined line has the
    number of its first line. As in pip, a comment on a line ending in
    `\\` takes the lines joined to it along."""
    joined, pending, first = [], [], None
    for number, line in enumerate(text.splitlines(), 1):
        comment = COMMENT.match(line)
        if line.endswith("\\") and not comment:
            if not pending:
                first = number
            pending.append(line.strip("\\"))
            continue
        if comment:
            line = f" {line}"  # stays a comment once joined
        if pending:
            joined.append((first, "".join(pending) + line))
            pending = []
        else:
            joined.append((number, line))
    if pending:
        joined.append((first, "".join(pending)))
    stripped = [(number, COMMENT.sub("", line).strip()) for number, line in joined]
    return [(number, line) for number, line in stripped if line]


def split_options(line):
    """`line` split as pip splits it: the requirement, the words before the
    first word that starts with `-`, and the options, the words from it on
    (words are separated by single spaces)."""
    words = line.split(" ")
    start = next((i for i, word in enumerate(words) if word.startswith("-")), len(words))
    return " ".join(words[:start]), " ".join(words[start:])


def editable_name(value):
    """The project name of the editable `value` (the argument of `-e`): the
    name of its `#egg=` fragment, the only place pip finds one before it
    installs (it takes a local path or a VCS URL there, not `name @ url`),
    or None."""
    egg = EGG.search(value)
    name = egg and REQUIREMENT_NAME.match(egg.group(1))
    return name.group(0) if name else None


def requirement_names(path, root, problems, seen=None, constraints=False):
    """The project names that pip would install from the requirements file
    `path`, as `(name, file)` pairs; `constraints` reads it as a
    constraints file (`-c`), whose own requirements and editables pip does
    not install. Each line is read as pip reads it (`logical_lines`,
    `split_options`, `LineOptions`); its requirement, each editable, and
    each include are all read, where pip reads only the first of them.
    What cannot be named or read goes to `problems`, the file named
    relative to `root`: a requirement without a project name (a bare URL
    or path), an editable without an `#egg=` name, an include that is a
    URL or does not exist, options pip would reject, and words pip would
    ignore after an option. `seen` holds the files read, resolved and with
    how they were read, so include cycles stop whatever path names them."""
    seen = set() if seen is None else seen
    path = path.resolve()
    if (path, constraints) in seen:
        return []
    seen.add((path, constraints))
    found = []
    for number, line in logical_lines(path.read_text(encoding="utf-8")):
        where = f"{shown(path, root)}:{number}"
        requirement, options = split_options(line)
        try:
            opts, ignored = LineOptions().parse_args(shlex.split(options))
        except ValueError as e:
            problems.append(f"{where}: cannot split the options {options!r}: {e}")
            continue
        except OptionError as e:
            problems.append(f"{where}: pip rejects the line: {e}")
            continue
        if ignored:
            problems.append(
                f"{where}: pip ignores {' '.join(ignored)!r} after an option; "
                "give each requirement its own line"
            )
        if requirement and not constraints:
            if name := REQUIREMENT_NAME.match(requirement):
                found.append((normalize(name.group(0)), path))
            else:
                problems.append(
                    f"{where}: no project name in {requirement!r}; write it as `name @ url`"
                )
        for editable in [] if constraints else opts.editable or []:
            if name := editable_name(editable):
                found.append((normalize(name), path))
            else:
                problems.append(
                    f"{where}: no project name in the editable {editable!r}; "
                    "give it one with #egg=name"
                )
        includes = [(f, False) for f in opts.requirement or []]
        includes += [(f, True) for f in opts.constraint or []]
        for include, as_constraints in includes:
            target = path.parent / include
            if URL.match(include):
                problems.append(f"{where}: includes {include}, a URL, which the check cannot read")
            elif target.is_file():
                found += requirement_names(target, root, problems, seen, as_constraints)
            else:
                problems.append(f"{where}: includes {include}, which does not exist")
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


def detector_program(config):
    """The id of the program that the `[detector]` table of the parsed
    studio config `config` runs, the first element of its `command`:
    `native:` and its file name, `native:python` for a Python interpreter
    (`PYTHON`); None without a command."""
    detector = config.get("detector")
    command = detector.get("command") if isinstance(detector, dict) else None
    if not isinstance(command, list) or not command or not isinstance(command[0], str):
        return None
    name = PurePosixPath(command[0]).name
    if not name:
        return None
    return "native:python" if PYTHON.fullmatch(name) else f"native:{name}"


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
        if program := detector_program(config):
            add(program, path)
    for path in sorted([*root.glob("crates/*/Cargo.toml"), *root.glob("apps/*/Cargo.toml")]):
        try:
            manifest = tomllib.loads(path.read_text(encoding="utf-8"))
        except tomllib.TOMLDecodeError as e:
            problems.append(f"{path.relative_to(root)}: cannot be parsed: {e}")
            continue
        if depends_on_ort(manifest):
            add("native:onnxruntime", path)
    for path in sorted([*root.glob("crates/*/src/**/*.rs"), *root.glob("apps/*/src/**/*.rs")]):
        source = path.read_text(encoding="utf-8", errors="replace")
        for component, literal in FIXED_PROGRAMS:
            if literal.search(source):
                add(component, path)
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
