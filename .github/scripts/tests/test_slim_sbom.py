"""slim_sbom.py's pieces (RFC 0041 §6.3).

`syft-docs-lock.cdx.json` is syft 1.56.0's answer to the command
`vuln-scan.yaml` runs, `syft scan file:docs/pnpm-lock.yaml -o cyclonedx-json`,
recorded on 2026-11-08 and cut to its first four components plus the one it
gives no purl (the lockfile itself, a `file` component). The 513-component
tree is built from that recorded component's shape.
"""

import contextlib
import copy
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import slim_sbom  # noqa: E402

RECORDED = json.loads((Path(__file__).resolve().parent / "fixtures" / "slim_sbom" / "syft-docs-lock.cdx.json").read_text())


def tree(n: int) -> dict:
    """`n` distinct npm components shaped like syft's, plus the lockfile's `file` component."""
    template = RECORDED["components"][0]
    doc = copy.deepcopy(RECORDED)
    doc["components"] = []
    for i in range(n):
        c = copy.deepcopy(template)
        c["name"], c["purl"], c["bom-ref"] = f"pkg-{i}", f"pkg:npm/pkg-{i}@1.0.{i}", f"pkg:npm/pkg-{i}@1.0.{i}?package-id={i:016x}"
        doc["components"].append(c)
    doc["components"].append(next(c for c in RECORDED["components"] if not c.get("purl")))
    return doc


class SlimSbom(unittest.TestCase):
    def setUp(self):
        self.prefix = str(Path(tempfile.mkdtemp()) / "sbom-docs")

    def run_main(self, doc: dict, *argv: str) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        args = ["slim_sbom.py", "--out-prefix", self.prefix, *argv]
        with (
            mock.patch.object(sys, "argv", args),
            mock.patch.object(sys, "stdin", io.StringIO(json.dumps(doc))),
            contextlib.redirect_stdout(out),
            contextlib.redirect_stderr(err),
        ):
            code = slim_sbom.main()
        return code, out.getvalue(), err.getvalue()

    def pieces(self, out: str) -> list[dict]:
        return [json.loads(Path(p).read_text()) for p in out.splitlines()]

    def test_recorded_sbom_keeps_coordinates_and_drops_the_lockfile(self):
        code, out, err = self.run_main(RECORDED)
        self.assertEqual(code, 0)
        self.assertEqual(out, f"{self.prefix}.1.cdx.json\n")
        self.assertEqual(err, f"{self.prefix}: 4 components in 1 piece(s)\n")
        [piece] = self.pieces(out)
        self.assertEqual((piece["bomFormat"], piece["specVersion"], piece["version"]), ("CycloneDX", "1.6", 1))
        self.assertEqual(
            piece["components"][0],
            {"type": "library", "name": "@algolia/abtesting", "version": "1.27.0", "purl": "pkg:npm/%40algolia/abtesting@1.27.0"},
        )
        self.assertNotIn("/pnpm-lock.yaml", [c["name"] for c in piece["components"]])

    def test_513_components_make_two_pieces_and_drop_nothing(self):
        code, out, err = self.run_main(tree(513))
        self.assertEqual(code, 0)
        self.assertEqual(out.splitlines(), [f"{self.prefix}.1.cdx.json", f"{self.prefix}.2.cdx.json"])
        self.assertEqual(err, f"{self.prefix}: 513 components in 2 piece(s)\n")
        first, second = self.pieces(out)
        self.assertEqual((len(first["components"]), len(second["components"])), (512, 1))
        purls = [c["purl"] for p in (first, second) for c in p["components"]]
        self.assertEqual(purls, [f"pkg:npm/pkg-{i}@1.0.{i}" for i in range(513)])

    def test_exactly_512_is_one_piece(self):
        code, out, _ = self.run_main(tree(512))
        self.assertEqual(code, 0)
        self.assertEqual(len(out.splitlines()), 1)

    def test_empty_tree_still_writes_one_empty_piece(self):
        for doc in ({"bomFormat": "CycloneDX", "specVersion": "1.7"}, {"components": None}, tree(0)):
            with self.subTest(doc=doc):
                code, out, err = self.run_main(doc)
                self.assertEqual(code, 0)
                [piece] = self.pieces(out)
                self.assertEqual(piece["components"], [])
                self.assertIn(": 0 components in 1 piece(s)", err)

    def test_chunk_option(self):
        code, out, err = self.run_main(tree(5), "--chunk", "2")
        self.assertEqual(code, 0)
        self.assertEqual([len(p["components"]) for p in self.pieces(out)], [2, 2, 1])
        self.assertIn(": 5 components in 3 piece(s)", err)

    def test_chunk_below_one_is_a_usage_error(self):
        for chunk in ("0", "-1"):
            with self.subTest(chunk=chunk), self.assertRaises(SystemExit) as cm:
                self.run_main(tree(5), "--chunk", chunk)
            self.assertEqual(cm.exception.code, 2)

    def test_missing_out_prefix_is_a_usage_error(self):
        with mock.patch.object(sys, "argv", ["slim_sbom.py"]), contextlib.redirect_stderr(io.StringIO()) as err:
            with self.assertRaises(SystemExit) as cm:
                slim_sbom.main()
        self.assertEqual(cm.exception.code, 2)
        self.assertIn("--out-prefix", err.getvalue())

    def test_input_that_is_not_json_fails(self):
        with (
            mock.patch.object(sys, "argv", ["slim_sbom.py", "--out-prefix", self.prefix]),
            mock.patch.object(sys, "stdin", io.StringIO("syft: command not found\n")),
        ):
            with self.assertRaises(json.JSONDecodeError):
                slim_sbom.main()


if __name__ == "__main__":
    unittest.main()
