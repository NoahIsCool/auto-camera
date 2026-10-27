"""Tests for `module_size.py` on small planted trees (no repository state)."""

import contextlib
import io
import tempfile
import unittest
from pathlib import Path

import module_size


def lines(n, prefix="// line"):
    """Rust source of exactly `n` lines."""
    return "".join(f"{prefix} {i}\n" for i in range(n))


class Tree:
    """A temporary repository: files are written relative to its root."""

    def __init__(self):
        self._dir = tempfile.TemporaryDirectory()
        self.root = Path(self._dir.name)
        self.write("crates/demo/Cargo.toml", "[package]\nname = \"demo\"\n")

    def write(self, rel, text):
        """Writes `text` to `rel`, creating its directories."""
        path = self.root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def offenders(self, limit=module_size.LIMIT):
        """The offenders `check` reports, as relative path strings."""
        offenders, _ = module_size.check(self.root, limit)
        return [str(p) for p, _ in offenders]

    def counted(self):
        """Every module `check` counts, as relative path strings."""
        _, sizes = module_size.check(self.root)
        return sorted(str(p) for p, _ in sizes)

    def main(self, *args):
        """Runs the command line on the tree; returns (status, output)."""
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            status = module_size.main([str(self.root), *args])
        return status, out.getvalue()

    def close(self):
        """Removes the tree."""
        self._dir.cleanup()


class ModuleSizeTest(unittest.TestCase):
    """The budget, what counts as a test module, and the command line."""

    def setUp(self):
        self.tree = Tree()
        self.tree.write("crates/demo/src/lib.rs", "//! Demo.\n\nmod big;\n")

    def tearDown(self):
        self.tree.close()

    def test_a_planted_601_line_module_fails(self):
        self.tree.write("crates/demo/src/big.rs", lines(601))
        self.assertEqual(self.tree.offenders(), ["crates/demo/src/big.rs"])
        status, out = self.tree.main()
        self.assertEqual(status, 1)
        self.assertIn("crates/demo/src/big.rs: 601 lines", out)

    def test_a_600_line_module_passes(self):
        self.tree.write("crates/demo/src/big.rs", lines(600))
        self.assertEqual(self.tree.offenders(), [])
        status, out = self.tree.main()
        self.assertEqual(status, 0)
        self.assertIn("crates/demo/src/big.rs (600)", out)

    def test_the_limit_can_be_changed(self):
        self.tree.write("crates/demo/src/big.rs", lines(101))
        self.assertEqual(self.tree.main("--limit", "100")[0], 1)
        self.assertEqual(self.tree.main("--limit", "101")[0], 0)

    def test_a_missing_root_is_a_usage_error(self):
        with contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as stop:
                module_size.main([str(self.tree.root / "nowhere")])
        self.assertEqual(stop.exception.code, 2)

    def test_lines_are_counted_as_an_editor_shows_them(self):
        self.assertEqual(module_size.count_lines(b""), 0)
        self.assertEqual(module_size.count_lines(b"a"), 1)
        self.assertEqual(module_size.count_lines(b"a\n"), 1)
        self.assertEqual(module_size.count_lines(b"a\nb"), 2)
        self.assertEqual(module_size.count_lines(b"\n\n"), 2)

    def test_inline_tests_count_towards_their_file(self):
        code = lines(500) + "#[cfg(test)]\nmod tests {\n" + lines(99, "    //") + "}\n"
        self.tree.write("crates/demo/src/big.rs", code)
        self.assertEqual(self.tree.offenders(), ["crates/demo/src/big.rs"])

    def test_tests_files_and_tests_directories_are_exempt(self):
        self.tree.write("crates/demo/src/big.rs", "#[cfg(test)]\nmod tests;\n")
        self.tree.write("crates/demo/src/big/tests.rs", lines(900))
        self.tree.write("crates/demo/src/big/tests/framing.rs", lines(900))
        self.tree.write("crates/demo/tests/engine.rs", lines(900))
        self.tree.write("crates/demo/tests/common/mod.rs", lines(900))
        self.assertEqual(self.tree.offenders(), [])
        self.assertEqual(self.tree.counted(), ["crates/demo/src/big.rs", "crates/demo/src/lib.rs"])

    def test_modules_declared_for_tests_only_are_exempt(self):
        self.tree.write(
            "crates/demo/src/lib.rs",
            "mod big;\n#[cfg(test)]\n// helpers\n\nmod testkit;\n"
            "#[cfg(all(test, feature = \"x\"))]\npub(crate) mod fixtures;\n",
        )
        self.tree.write(
            "crates/demo/src/big.rs",
            "#[cfg(test)]\n#[path = \"big_tests.rs\"]\nmod tests;\n#[cfg(test)]\nmod support;\n",
        )
        for rel in [
            "crates/demo/src/testkit.rs",
            "crates/demo/src/testkit/deep.rs",
            "crates/demo/src/fixtures/mod.rs",
            "crates/demo/src/big_tests.rs",
            "crates/demo/src/big/support.rs",
        ]:
            self.tree.write(rel, lines(900))
        self.tree.write("crates/demo/src/inner.rs", "#![cfg(test)]\n" + lines(900))
        self.assertEqual(self.tree.offenders(), [])

    def test_other_cfgs_and_stray_attributes_do_not_exempt(self):
        self.tree.write(
            "crates/demo/src/lib.rs",
            "#[cfg(not(test))]\nmod big;\n#[cfg(test)]\nuse std::fmt;\nmod other;\n"
            "#[cfg(any(test, feature = \"x\"))]\nmod third;\n",
        )
        for name in ["big", "other", "third"]:
            self.tree.write(f"crates/demo/src/{name}.rs", lines(601))
        self.assertEqual(
            sorted(self.tree.offenders()),
            ["crates/demo/src/big.rs", "crates/demo/src/other.rs", "crates/demo/src/third.rs"],
        )

    def test_a_test_module_does_not_exempt_its_crate_roots_siblings(self):
        self.tree.write("crates/demo/src/bin/tool.rs", "#[cfg(test)]\nmod checks;\n")
        self.tree.write("crates/demo/src/bin/checks.rs", lines(900))
        self.tree.write("crates/demo/src/bin/other.rs", lines(601))
        self.tree.write("crates/demo/examples/show.rs", "#[cfg(test)]\nmod t;\n" + lines(601))
        self.tree.write("crates/demo/examples/t.rs", lines(900))
        self.assertEqual(
            sorted(self.tree.offenders()),
            ["crates/demo/examples/show.rs", "crates/demo/src/bin/other.rs"],
        )

    def test_build_output_and_hidden_directories_are_skipped(self):
        self.tree.write("target/debug/build/out.rs", lines(900))
        self.tree.write("crates/demo/target/gen.rs", lines(900))
        self.tree.write(".git/hooks/x.rs", lines(900))
        self.assertEqual(self.tree.counted(), ["crates/demo/src/lib.rs"])


if __name__ == "__main__":
    unittest.main()
