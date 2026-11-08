"""vex.py check and render (RFC 0041 §6.3).

`fixtures/vex/batlehub.openvex.json` and `trivyignore-one-entry.yaml` are the
repository's `vex/batlehub.openvex.json` and `.trivyignore.yaml` as they stood
at 756158f5^ — the last commit where the gate carried an entry
(CVE-2026-116587, stated `not_affected`). `trivyignore-empty.yaml` is the
`.trivyignore.yaml` of 756158f5, which removed it. Each failing case breaks one
thing in a copy of the recorded document.
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
import vex  # noqa: E402

FIXTURES = Path(__file__).resolve().parent / "fixtures" / "vex"
DOC = FIXTURES / "batlehub.openvex.json"
ONE_ENTRY = FIXTURES / "trivyignore-one-entry.yaml"
EMPTY = FIXTURES / "trivyignore-empty.yaml"
RECORDED = json.loads(DOC.read_text())
NOT_AFFECTED = "CVE-2026-116587"
WORKER = "ghcr.io/batlehub/batlehub-worker@sha256:" + "a" * 64
PROXY = "ghcr.io/batlehub/batlehub@sha256:" + "b" * 64


class Vex(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())

    def run_main(self, *argv: str) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with mock.patch.object(sys, "argv", ["vex.py", *argv]), contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = vex.main()
        return code, out.getvalue(), err.getvalue()

    def check(self, doc: dict | Path, trivyignore: Path = ONE_ENTRY) -> tuple[int, str, str]:
        if isinstance(doc, dict):
            path = self.tmp / "doc.json"
            path.write_text(json.dumps(doc))
            doc = path
        return self.run_main("check", "--doc", str(doc), "--trivyignore", str(trivyignore))

    def statement(self, doc: dict, cve: str) -> dict:
        return next(s for s in doc["statements"] if s["vulnerability"]["name"] == cve)

    # ── check ────────────────────────────────────────────────────────────────

    def test_recorded_document_passes(self):
        code, out, err = self.check(DOC)
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out, f"✓ {DOC}: 6 statement(s), 1 matched against {ONE_ENTRY}\n")

    def test_not_affected_without_an_ignore_entry_fails(self):
        code, _, err = self.check(DOC, EMPTY)
        self.assertEqual(code, 1)
        self.assertIn(f"✗ {NOT_AFFECTED} is stated not_affected but {EMPTY} does not ignore it", err)
        self.assertIn("1 problem(s) in the VEX document.", err)

    def test_fixed_without_an_ignore_entry_passes(self):
        doc = copy.deepcopy(RECORDED)
        st = self.statement(doc, NOT_AFFECTED)
        st["status"] = "fixed"
        code, out, _ = self.check(doc, EMPTY)
        self.assertEqual(code, 0)
        self.assertIn("6 statement(s), 0 matched", out)

    def test_a_fixed_duplicate_does_not_hide_a_not_affected_statement(self):
        doc = copy.deepcopy(RECORDED)
        fixed = copy.deepcopy(self.statement(doc, NOT_AFFECTED))
        fixed["status"] = "fixed"
        doc["statements"].insert(0, fixed)
        code, _, err = self.check(doc, EMPTY)
        self.assertEqual(code, 1)
        self.assertIn(f"{NOT_AFFECTED} is stated not_affected", err)

    def test_ignore_entry_without_a_statement_fails(self):
        doc = copy.deepcopy(RECORDED)
        doc["statements"] = [s for s in doc["statements"] if s["vulnerability"]["name"] != NOT_AFFECTED]
        code, _, err = self.check(doc)
        self.assertEqual(code, 1)
        self.assertIn(f"✗ {NOT_AFFECTED} is suppressed in {ONE_ENTRY} but has no VEX statement", err)

    def test_absent_trivyignore_is_no_ignores(self):
        code, _, err = self.check(DOC, self.tmp / "nope.yaml")
        self.assertEqual(code, 1)
        self.assertIn(f"{NOT_AFFECTED} is stated not_affected", err)

    def test_each_statement_rule(self):
        cases = {
            "no vulnerability name": lambda s: s.update(vulnerability={}),
            "status 'maybe' is not one of": lambda s: s.update(status="maybe"),
            "no timestamp — when was this decided?": lambda s: s.pop("timestamp"),
            "names no product, so it applies to nothing": lambda s: s.update(products=[]),
            "a product has no @id": lambda s: s["products"][0].pop("@id"),
            "justification 'it_is_fine' is not one of": lambda s: s.update(justification="it_is_fine"),
            "not_affected with no impact_statement": lambda s: s.pop("impact_statement"),
            "affected with no action_statement": lambda s: (s.update(status="affected"), s.pop("action_statement")),
        }
        for message, breaks in cases.items():
            with self.subTest(message):
                doc = copy.deepcopy(RECORDED)
                breaks(self.statement(doc, NOT_AFFECTED))
                code, out, err = self.check(doc)
                self.assertEqual((code, out), (1, ""))
                self.assertIn(message, err)

    def test_document_header_rules(self):
        doc = copy.deepcopy(RECORDED)
        doc["@context"] = "https://example.com/vex"
        for field in ("@id", "author", "timestamp", "version"):
            doc.pop(field)
        code, _, err = self.check(doc)
        self.assertEqual(code, 1)
        self.assertIn("✗ @context is not an OpenVEX namespace", err)
        for field in ("@id", "author", "timestamp", "version"):
            self.assertIn(f"✗ the document has no {field}", err)
        self.assertIn("5 problem(s)", err)

    def test_no_statements_fails(self):
        for statements in ([], None, {"not": "a list"}):
            with self.subTest(statements=statements):
                code, _, err = self.check({**RECORDED, "statements": statements})
                self.assertEqual(code, 1)
                self.assertIn("✗ the document carries no statements", err)

    def test_missing_or_invalid_document_exits(self):
        bad = self.tmp / "bad.json"
        bad.write_text("{")
        for path, message in ((self.tmp / "nope.json", "does not exist"), (bad, "is not valid JSON")):
            with self.subTest(message):
                with self.assertRaises(SystemExit) as cm:
                    self.check(path)
                self.assertTrue(str(cm.exception.code).startswith(f"{path} {message}"), cm.exception.code)

    def test_trivyignore_ids_reads_the_real_shape(self):
        self.assertEqual(vex.trivyignore_ids(ONE_ENTRY), {NOT_AFFECTED})
        self.assertEqual(vex.trivyignore_ids(EMPTY), set())

    # ── render ───────────────────────────────────────────────────────────────

    def test_render_binds_products_to_digests_and_drops_the_rest(self):
        out_path = self.tmp / "out" / "batlehub-1.2.3.openvex.json"
        code, out, err = self.run_main("render", "--doc", str(DOC), "--version", "1.2.3", "--image", WORKER, "--image", PROXY, "--out", str(out_path))
        self.assertEqual(code, 0)
        self.assertEqual(out, f"✓ {out_path}: 6 statement(s) for 1.2.3\n")
        self.assertIn("dropping product pkg:oci/batlehub-worker-guarddog from CVE-2026-84304: this release published no such image", err)
        rendered = json.loads(out_path.read_text())
        self.assertEqual(rendered["@id"], RECORDED["@id"] + "-1.2.3")
        self.assertNotEqual(rendered["timestamp"], RECORDED["timestamp"])
        worker_purl = f"pkg:oci/batlehub-worker@sha256:{'a' * 64}?repository_url=ghcr.io/batlehub/batlehub-worker"
        self.assertEqual([p["@id"] for p in self.statement(rendered, NOT_AFFECTED)["products"]], [worker_purl])
        self.assertEqual(self.statement(rendered, NOT_AFFECTED)["products"][0]["subcomponents"], self.statement(RECORDED, NOT_AFFECTED)["products"][0]["subcomponents"])

    def test_render_with_no_matching_image_writes_an_empty_document(self):
        out_path = self.tmp / "empty.json"
        image = "ghcr.io/batlehub/other@sha256:" + "c" * 64
        code, out, _ = self.run_main("render", "--doc", str(DOC), "--version", "1.2.3", "--image", image, "--out", str(out_path))
        self.assertEqual(code, 0)
        self.assertIn("no statement applies to the images given — writing an empty document", out)
        self.assertEqual(json.loads(out_path.read_text())["statements"], [])

    def test_render_refuses_a_tag(self):
        with self.assertRaises(SystemExit) as cm:
            self.run_main("render", "--doc", str(DOC), "--version", "1.2.3", "--image", "ghcr.io/batlehub/batlehub:1.2.3", "--out", str(self.tmp / "x.json"))
        self.assertIn("must be <repository>@sha256:… (a digest, not a tag)", str(cm.exception.code))
        self.assertFalse((self.tmp / "x.json").exists())

    def test_usage_errors(self):
        for argv in ([], ["render", "--version", "1", "--out", "x"], ["lint"]):
            with self.subTest(argv=argv), contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as cm:
                    self.run_main(*argv)
                self.assertEqual(cm.exception.code, 2)


if __name__ == "__main__":
    unittest.main()
