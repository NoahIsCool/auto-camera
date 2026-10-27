#!/usr/bin/env python3
"""Fails when a non-test Rust module is longer than the line budget.

CLAUDE.md calls a module over about 600 lines a smell; this check makes the
budget a CI gate. Every `.rs` file of the repository that is not a test
module may have at most `LIMIT` (600) lines. A file is one module, together
with the inline modules it contains, so its length is the file's: an inline
`#[cfg(test)] mod tests { ... }` counts towards its file, and the way out is
the repository's usual one, a `tests.rs` beside it.

A file is a test module, and exempt, when

* its name is `tests.rs`;
* it lies under a directory named `tests`: a crate's integration tests, and
  the children of a `tests.rs` (such as `core/tests/framing.rs`);
* its `mod` item carries `#[cfg(test)]` (or `#[cfg(all(test, ...))]`), as in
  `#[cfg(test)] mod testkit;` or `#[cfg(test)] #[path = "x_tests.rs"]
  mod tests;`, or the file's own inner attributes, which Rust allows only
  before its first item (after comments and `//!` docs), include
  `#![cfg(test)]` (or `#![cfg(all(test, ...))]`); the children of such a
  module are test modules too.

Only `mod name;` items at the top level of a file are followed (not the
file children of an inline module, nor the children of a `#[path]`
module), with one-line attributes on the lines just before the item,
comments and blank lines aside. Directories named `target` and hidden
directories are skipped.

Usage: `python3 scripts/module_size.py [ROOT] [--limit N]`. Exits 0 when
every module fits, 1 when one does not (each is listed), 2 on bad usage.
Standard library only.
"""

import argparse
import re
import sys
from pathlib import Path

#: Longest non-test module accepted, in lines.
LIMIT = 600

#: A `mod name;` item (a module in its own file), with any visibility.
MOD_ITEM = re.compile(r"^(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;")
#: An outer attribute on a line of its own; group 1 is its content.
OUTER_ATTR = re.compile(r"^#\[(.*)\]$")
#: A `#[path = "..."]` attribute's content; group 1 is the path.
PATH_ATTR = re.compile(r'^path\s*=\s*"([^"]+)"$')
#: The opening of an inner attribute, `#![`, with any spacing.
INNER_ATTR_OPEN = re.compile(r"#!\s*\[")
#: File names whose children live beside them rather than in a directory of
#: their own (crate roots and `mod.rs`).
DIR_OWNERS = {"lib.rs", "main.rs", "mod.rs", "build.rs"}
#: Directories beside a `Cargo.toml` whose files are crate roots.
ROOT_DIRS = {"examples", "benches", "tests"}


def count_lines(data):
    """Lines in `data` (bytes) as an editor shows them: a last line without
    a newline still counts, an empty file has none."""
    if not data:
        return 0
    return data.count(b"\n") + (0 if data.endswith(b"\n") else 1)


def rust_files(root):
    """Every `.rs` file under `root`, skipping `target` and hidden
    directories, in a stable order."""
    found = []
    stack = [root]
    while stack:
        directory = stack.pop()
        for entry in sorted(directory.iterdir()):
            if entry.is_dir():
                if entry.name != "target" and not entry.name.startswith("."):
                    stack.append(entry)
            elif entry.suffix == ".rs" and entry.is_file():
                found.append(entry)
    return sorted(found)


def is_crate_root(path):
    """True when `path` is the root file of a crate target: a library or
    binary (`lib.rs`, `main.rs`, `src/bin/*.rs`), a build script, an
    example, a bench or an integration test."""
    parent = path.parent
    return (
        path.name in DIR_OWNERS
        or (parent.name == "bin" and parent.parent.name == "src")
        or (parent.name in ROOT_DIRS and (parent.parent / "Cargo.toml").is_file())
    )


def children_dir(path):
    """The directory holding the files of the modules that `path` declares."""
    if is_crate_root(path):
        return path.parent
    return path.parent / path.stem


def is_cfg_test(attr):
    """True when the attribute content `attr` compiles its item only for
    tests: `cfg(test)`, or `cfg(all(...))` with `test` among its terms."""
    flat = re.sub(r"\s+", "", attr)
    if flat == "cfg(test)":
        return True
    if flat.startswith("cfg(all(") and flat.endswith("))"):
        terms, depth, term = [], 0, ""
        for c in flat[len("cfg(all(") : -2]:
            if c == "," and depth == 0:
                terms.append(term)
                term = ""
                continue
            depth += {"(": 1, ")": -1}.get(c, 0)
            term += c
        terms.append(term)
        return "test" in terms
    return False


def skip_block_comment(text, start):
    """The index just past the block comment opening at `start` in `text`
    (Rust block comments nest), or `len(text)` when it never closes."""
    depth, i = 0, start
    while i < len(text):
        pair = text[i : i + 2]
        if pair == "/*":
            depth += 1
            i += 2
        elif pair == "*/":
            depth -= 1
            i += 2
            if depth == 0:
                return i
        else:
            i += 1
    return len(text)


def attribute_end(text, start):
    """The index of the `]` that closes the attribute whose `[` is at
    `start` in `text`, skipping nested brackets and string literals, or
    `None` when it never closes."""
    depth, i = 0, start
    while i < len(text):
        c = text[i]
        if c == '"':
            i += 1
            while i < len(text) and text[i] != '"':
                i += 2 if text[i] == "\\" else 1
        elif c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
            if depth == 0:
                return i
        i += 1
    return None


def file_is_cfg_test(text):
    """True when the source `text` compiles its whole file only for tests:
    one of its inner attributes is `#![cfg(test)]` (or `cfg(all(...))` with
    `test`). Inner attributes come before the first item, among comments
    and `//!` or `/*! */` docs, so the scan stops at the first thing that is
    none of these."""
    i = 0
    while True:
        while i < len(text) and text[i].isspace():
            i += 1
        if text.startswith("//", i):
            newline = text.find("\n", i)
            i = len(text) if newline < 0 else newline + 1
        elif text.startswith("/*", i):
            i = skip_block_comment(text, i)
        elif opening := INNER_ATTR_OPEN.match(text, i):
            bracket = opening.end() - 1
            end = attribute_end(text, bracket)
            if end is None:
                return False
            if is_cfg_test(text[bracket + 1 : end]):
                return True
            i = end + 1
        else:
            return False


def test_only_modules(path, text):
    """The files of the modules that `path` (with source `text`) declares
    for tests only, by `#[cfg(test)]` on their top-level `mod` items."""
    found = []
    attrs = []
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("//"):
            continue
        attr = OUTER_ATTR.match(line) if raw.startswith("#") else None
        if attr:
            attrs.append(attr.group(1))
            continue
        item = MOD_ITEM.match(raw)
        if item and any(is_cfg_test(a) for a in attrs):
            explicit = [m.group(1) for a in attrs if (m := PATH_ATTR.match(a.strip()))]
            if explicit:
                found.append(path.parent / explicit[-1])
            else:
                base = children_dir(path)
                name = item.group(1)
                found.append(base / f"{name}.rs")
                found.append(base / name / "mod.rs")
        attrs = []
    return found


def is_under(path, directory):
    """True when `path` lies inside `directory`."""
    try:
        path.relative_to(directory)
    except ValueError:
        return False
    return True


def check(root, limit=LIMIT):
    """Scans `root` and returns `(offenders, sizes)`: the non-test modules
    longer than `limit` lines as `(path, lines)`, longest first, and the
    sizes of every non-test module, as paths relative to `root`."""
    root = root.resolve()
    files = rust_files(root)
    texts = {f: f.read_bytes() for f in files}
    test_files = set()
    test_dirs = set()
    for f, data in texts.items():
        text = data.decode("utf-8", errors="replace")
        if file_is_cfg_test(text):
            test_files.add(f)
        for module in test_only_modules(f, text):
            test_files.add(module)
            # A declared module is never a crate root: `x.rs` keeps its
            # children in `x/`, `x/mod.rs` beside it.
            test_dirs.add(module.parent if module.name == "mod.rs" else module.parent / module.stem)
    sizes = []
    for f, data in texts.items():
        rel = f.relative_to(root)
        if f.name == "tests.rs" or "tests" in rel.parts[:-1] or f in test_files:
            continue
        if any(is_under(f, d) for d in test_dirs):
            continue
        sizes.append((rel, count_lines(data)))
    sizes.sort(key=lambda s: (-s[1], str(s[0])))
    offenders = [s for s in sizes if s[1] > limit]
    return offenders, sizes


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
    parser.add_argument("--limit", type=int, default=LIMIT, help="most lines a module may have")
    args = parser.parse_args(argv)
    if not args.root.is_dir():
        parser.error(f"{args.root} is not a directory")
    offenders, sizes = check(args.root, args.limit)
    if offenders:
        for path, lines in offenders:
            print(f"{path}: {lines} lines, over the {args.limit}-line module budget")
        print(
            "Split each along its sections, or move a long inline test module to "
            "tests.rs (scripts/module_size.py says what counts as a test module)."
        )
        return 1
    largest = ", ".join(f"{path} ({lines})" for path, lines in sizes[:3])
    print(f"{len(sizes)} non-test modules, none over {args.limit} lines; largest: {largest}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
