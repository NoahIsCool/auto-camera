"""Tests for `licence_manifest.py` on small planted trees (no repository state)."""

import contextlib
import io
import tempfile
import unittest
from pathlib import Path

import licence_manifest

#: The manifest's header and separator rows.
HEADER = "| Id | Component | Licence | Licence source | Checked |\n|---|---|---|---|---|\n"


def row(ids, licence="MIT", source="[text](https://example.org/LICENSE)", checked="yes"):
    """One manifest row for the ids `ids` (a string of backquoted ids)."""
    return f"| {ids} | A component | {licence} | {source} | {checked} |\n"


#: A manifest with a valid row for every component `Tree.complete` plants.
COMPLETE = (
    "# Manifest\n\n"
    + HEADER
    + row("`native:ffmpeg`", "LGPL-2.1-or-later")
    + row("`native:onnxruntime`")
    + row("`model:owl`, `model:sam`", "Apache-2.0", checked="partly")
    + row("`pypi:mlx`")
    + "\nNotes after the table.\n"
)


class Tree:
    """A temporary repository: files are written relative to its root."""

    def __init__(self):
        self._dir = tempfile.TemporaryDirectory()
        self.root = Path(self._dir.name)

    def write(self, rel, text):
        """Writes `text` to `rel`, creating its directories."""
        path = self.root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def complete(self):
        """Plants one component of each rule, and a manifest listing them."""
        self.write("sidecars/demo/requirements.txt", "mlx==0.32.3\n")
        self.write(
            "config/studio.toml",
            '[detector]\nkind = "owl"\nmodel = "../models/owl/detector.onnx"\n'
            'tokenizer = "../models/owl/tokenizer.json"\n',
        )
        self.write(
            "config/sidecar.toml",
            '[detector]\nkind = "sidecar"\n'
            'command = ["python", "server.py", "--model", "../models/sam"]\n',
        )
        self.write("crates/vision/Cargo.toml", '[dependencies]\nort = { version = "2" }\n')
        self.write("crates/sources/src/probe.rs", 'const TOOL: &str = "ffmpeg";\n')
        self.write("docs/licence-manifest.md", COMPLETE)

    def found(self):
        """The ids `check` finds."""
        return licence_manifest.check(self.root)[0]

    def missing(self):
        """The ids `check` reports without a row."""
        return [c for c, _ in licence_manifest.check(self.root)[1]]

    def problems(self):
        """The problems `check` reports."""
        return licence_manifest.check(self.root)[2]

    def main(self, *args):
        """Runs the command line on the tree; returns (status, output)."""
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            status = licence_manifest.main([str(self.root), *args])
        return status, out.getvalue()

    def close(self):
        """Removes the tree."""
        self._dir.cleanup()


class LicenceManifestTest(unittest.TestCase):
    """What counts as a component, what makes a row valid, and the command line."""

    def setUp(self):
        self.tree = Tree()
        self.tree.complete()

    def tearDown(self):
        self.tree.close()

    def test_a_complete_manifest_passes(self):
        self.assertEqual(
            self.tree.found(),
            ["model:owl", "model:sam", "native:ffmpeg", "native:onnxruntime", "pypi:mlx"],
        )
        self.assertEqual(self.tree.missing(), [])
        self.assertEqual(self.tree.problems(), [])
        status, out = self.tree.main()
        self.assertEqual(status, 0)
        self.assertIn("5 non-crate components, each with a licence entry", out)

    def test_a_new_requirement_without_an_entry_fails(self):
        self.tree.write("sidecars/demo/requirements.txt", "mlx==0.32.3\nPillow>=10\n")
        self.assertEqual(self.tree.missing(), ["pypi:pillow"])
        status, out = self.tree.main()
        self.assertEqual(status, 1)
        self.assertIn(
            "pypi:pillow has no licence entry in docs/licence-manifest.md "
            "(found in sidecars/demo/requirements.txt)",
            out,
        )

    def test_a_component_whose_row_is_removed_fails(self):
        self.tree.write("docs/licence-manifest.md", COMPLETE.replace(row("`pypi:mlx`"), ""))
        self.assertEqual(self.tree.missing(), ["pypi:mlx"])
        self.assertEqual(self.tree.main()[0], 1)

    def test_a_new_model_folder_without_an_entry_fails(self):
        self.tree.write(
            "config/other.json",
            '{"detector": {"kind": "owl", "model": "/data/models/yolo/model.onnx"}}',
        )
        self.assertEqual(self.tree.missing(), ["model:yolo"])

    def test_an_ort_dependency_needs_an_entry_under_any_name_or_target(self):
        self.tree.write(
            "docs/licence-manifest.md", COMPLETE.replace(row("`native:onnxruntime`"), "")
        )
        self.assertEqual(self.tree.missing(), ["native:onnxruntime"])
        self.tree.write("crates/vision/Cargo.toml", '[dependencies]\nrayon = "1"\n')
        self.assertEqual(self.tree.missing(), [])
        self.tree.write(
            "apps/app/Cargo.toml",
            "[target.'cfg(target_os = \"macos\")'.dependencies]\n"
            'onnx = { package = "ort", version = "2" }\n',
        )
        self.assertEqual(self.tree.missing(), ["native:onnxruntime"])

    def test_an_ffmpeg_literal_needs_an_entry(self):
        self.tree.write(
            "docs/licence-manifest.md",
            COMPLETE.replace(row("`native:ffmpeg`", "LGPL-2.1-or-later"), ""),
        )
        self.assertEqual(self.tree.missing(), ["native:ffmpeg"])
        self.tree.write("crates/sources/src/probe.rs", "// runs ffmpeg, but names no tool\n")
        self.assertEqual(self.tree.missing(), [])
        self.tree.write("apps/app/src/ui/deep.rs", 'let tool = "ffprobe";\n')
        self.assertEqual(self.tree.missing(), ["native:ffmpeg"])

    def test_requirement_lines_are_read_as_pip_reads_them(self):
        self.tree.write(
            "sidecars/demo/requirements.txt",
            "# a comment\n\n"
            "mlx==0.32.3  # pinned\n"
            "mlx-vlm @ git+https://github.com/Blaizzy/mlx-vlm.git@5b22c24#egg=mlx_vlm\n"
            "Foo_Bar.baz[extra] >= 1 ; python_version >= '3.10'\n"
            "--index-url https://pypi.org/simple\n"
            "-r more.txt\n",
        )
        self.tree.write("sidecars/demo/more.txt", "numpy\n--requirement=requirements.txt\n")
        self.assertEqual(
            [c for c in self.tree.found() if c.startswith("pypi:")],
            ["pypi:foo-bar-baz", "pypi:mlx", "pypi:mlx-vlm", "pypi:numpy"],
        )

    def test_a_missing_include_is_a_problem(self):
        self.tree.write("sidecars/demo/requirements.txt", "mlx\n-r gone.txt\n")
        self.assertEqual(
            self.tree.problems(),
            ["sidecars/demo/requirements.txt:2: includes gone.txt, which does not exist"],
        )
        self.assertEqual(self.tree.main()[0], 1)

    def test_a_requirement_without_a_project_name_is_a_problem(self):
        self.tree.write(
            "sidecars/demo/requirements.txt",
            "mlx\nhttps://example.org/pkg-1.0-py3-none-any.whl\n./vendored\n",
        )
        self.assertEqual(
            self.tree.problems(),
            [
                "sidecars/demo/requirements.txt:2: no project name in "
                "'https://example.org/pkg-1.0-py3-none-any.whl'; write it as `name @ url`",
                "sidecars/demo/requirements.txt:3: no project name in './vendored'; "
                "write it as `name @ url`",
            ],
        )

    def test_model_paths_name_their_folder(self):
        self.assertEqual(licence_manifest.model_folder("../models/owl/detector.onnx"), "owl")
        self.assertEqual(licence_manifest.model_folder("../models/sam31-mlx"), "sam31-mlx")
        self.assertEqual(licence_manifest.model_folder("model.onnx"), "model.onnx")
        config = {
            "detector": {
                "text_encoder": "m/a/text.onnx",
                "model": "",
                "command": ["py", "--model=m/b", "--model"],
            }
        }
        self.assertEqual(licence_manifest.detector_model_paths(config), ["m/a/text.onnx", "m/b"])
        self.assertEqual(licence_manifest.detector_model_paths({"detector": "owl"}), [])
        self.assertEqual(licence_manifest.detector_model_paths({}), [])

    def test_a_config_that_cannot_be_parsed_is_a_problem(self):
        self.tree.write("config/broken.toml", "[detector\n")
        problems = self.tree.problems()
        self.assertEqual(len(problems), 1)
        self.assertTrue(problems[0].startswith("config/broken.toml: cannot be parsed"))

    def test_invalid_rows_are_reported(self):
        self.tree.write(
            "docs/licence-manifest.md",
            COMPLETE + "\n" + HEADER + row("`pypi:other`"),  # a second table is not read
        )
        self.assertEqual(self.tree.problems(), [])
        rows = (
            row("no id")
            + row("`pypi:a b`")
            + row("`pypi:x`", licence="")
            + row("`pypi:y`", source="http://example.org")
            + row("`pypi:z`", checked="maybe")
            + row("`pypi:mlx`")
        )
        self.tree.write("docs/licence-manifest.md", COMPLETE.replace("\nNotes", rows + "\nNotes"))
        self.assertEqual(
            self.tree.problems(),
            [
                "docs/licence-manifest.md:9: no id in backquotes in the Id column",
                "docs/licence-manifest.md:10: `pypi:a b` is not kind:name "
                "(kind pypi, model or native)",
                "docs/licence-manifest.md:11: the Licence column is empty",
                "docs/licence-manifest.md:12: the Licence source column has no https:// link",
                "docs/licence-manifest.md:13: Checked is 'maybe', not one of yes, partly, no",
                "docs/licence-manifest.md:14: `pypi:mlx` is already in the row on line 8",
            ],
        )
        self.assertEqual(self.tree.main()[0], 1)

    def test_a_row_with_the_wrong_cell_count_is_reported(self):
        self.tree.write("docs/licence-manifest.md", COMPLETE + "| `pypi:mlx` | too few |\n")
        self.assertEqual(self.tree.problems(), [])  # after the table's end
        self.tree.write(
            "docs/licence-manifest.md", COMPLETE.replace("\nNotes", "| `pypi:q` | x |\n\nNotes")
        )
        self.assertEqual(self.tree.problems(), ["docs/licence-manifest.md:9: 2 cells, not 5"])

    def test_the_table_must_have_the_documented_columns(self):
        self.tree.write(
            "docs/licence-manifest.md", "| Id | Licence |\n|---|---|\n| `pypi:mlx` | MIT |\n"
        )
        self.assertEqual(
            self.tree.problems(),
            [
                "docs/licence-manifest.md:1: the table's columns must be "
                "Id | Component | Licence | Licence source | Checked"
            ],
        )
        self.tree.write("docs/licence-manifest.md", "No table here.\n")
        self.assertEqual(
            self.tree.problems(),
            ["docs/licence-manifest.md: no table whose header row starts with Id"],
        )

    def test_a_missing_manifest_fails(self):
        (self.tree.root / "docs/licence-manifest.md").unlink()
        self.assertEqual(self.tree.problems(), ["docs/licence-manifest.md: missing"])
        self.assertEqual(len(self.tree.missing()), 5)
        self.assertEqual(self.tree.main()[0], 1)

    def test_an_unused_entry_is_only_a_note(self):
        self.tree.write(
            "docs/licence-manifest.md", COMPLETE.replace("\nNotes", row("`model:old`") + "\nNotes")
        )
        status, out = self.tree.main()
        self.assertEqual(status, 0)
        self.assertIn(
            "note: model:old has a licence entry, but nothing in the repository uses it", out
        )

    def test_an_empty_repository_passes_with_an_empty_table(self):
        empty = Tree()
        try:
            empty.write("docs/licence-manifest.md", HEADER)
            self.assertEqual(licence_manifest.check(empty.root), ([], [], [], []))
        finally:
            empty.close()

    def test_a_root_that_is_not_a_directory_is_bad_usage(self):
        with contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as raised:
                licence_manifest.main([str(self.tree.root / "missing")])
        self.assertEqual(raised.exception.code, 2)


if __name__ == "__main__":
    unittest.main()
