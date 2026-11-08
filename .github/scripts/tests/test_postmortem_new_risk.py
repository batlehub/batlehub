"""postmortem_new_risk.py against answers recorded from postmortem 2.9.2.

The fixtures under fixtures/postmortem/ were written by the real binary on
2026-11-08 (RFC 0041 §17 M18): a base pinning minimist 1.2.6, lodash
4.17.21 and left-pad 1.3.0, and a head that adds event-stream 3.3.6 and the
malicious ynodejs_mod_log 10.0.1 and moves minimist to 1.2.5 and lodash to
4.17.20. `npm-tree-failed.*` is the same head with vuln.mlab.sh unreachable:
exit 0, an empty SARIF, and only the JSON's diagnostic to say so.
"""

import contextlib
import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import postmortem_new_risk as gate  # noqa: E402

FIX = Path(__file__).resolve().parent / "fixtures" / "postmortem"


def fixture(name):
    return json.loads((FIX / name).read_text())


class Gate(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        env = mock.patch.dict(os.environ, {}, clear=False)
        env.start()
        self.addCleanup(env.stop)
        os.environ.pop("VULN_MLAB_TOKEN", None)

    def write(self, name, doc):
        path = Path(self.tmp.name) / name
        path.write_text(doc if isinstance(doc, str) else json.dumps(doc))
        return str(path)

    def run_gate(self, *args):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = gate.main(["--root", "ui", *args])
        return code, out.getvalue()

    # --- pull request: block-new ---------------------------------------

    def test_an_added_or_changed_package_with_a_serious_advisory_fails(self):
        code, out = self.run_gate(
            "--tree", str(FIX / "npm-tree.json"), "--sarif", str(FIX / "npm-tree.sarif"),
            "--diff", str(FIX / "npm-diff.json"),
        )
        self.assertEqual(code, 1)
        self.assertIn("vulnerable dependency event-stream@3.3.6 — GHSA-mh6f-8j2x-4483 (critical)", out)
        self.assertIn("vulnerable dependency minimist@1.2.5 — GHSA-xvch-5gv4-984h (critical)", out)
        self.assertIn("vulnerable dependency lodash@4.17.20 — GHSA-35jh-r3h4-6jhm (high)", out)
        self.assertNotIn("GHSA-29mw-wpgm-hmr9", out, "a medium advisory is not blocking")
        self.assertIn("known-malicious package ynodejs_mod_log@10.0.1 (MAL-2023-1000) — remove it", out)

    def test_an_advisory_below_high_passes(self):
        tree = fixture("npm-tree.json")
        tree["vulnerabilities"] = [
            {"name": "lodash", "version": "4.17.20", "ecosystem": "npm",
             "vulns": [{"id": "GHSA-29mw-wpgm-hmr9", "severity": "medium"}]}
        ]
        code, out = self.run_gate("--tree", self.write("t.json", tree), "--diff", str(FIX / "npm-diff.json"))
        self.assertEqual(code, 0, out)

    def test_an_unrated_advisory_counts_as_high(self):
        tree = fixture("npm-tree.json")
        tree["vulnerabilities"] = [
            {"name": "lodash", "version": "4.17.20", "ecosystem": "npm",
             "vulns": [{"id": "RUSTSEC-2099-0001", "severity": "unknown"}]}
        ]
        code, out = self.run_gate("--tree", self.write("t.json", tree), "--diff", str(FIX / "npm-diff.json"))
        self.assertEqual(code, 1)
        self.assertIn("RUSTSEC-2099-0001 (unknown)", out)

    def test_an_advisory_on_an_unchanged_package_does_not_fail_a_pull_request(self):
        tree = fixture("npm-tree.json")
        tree["vulnerabilities"] = [
            {"name": "left-pad", "version": "1.3.0", "ecosystem": "npm",
             "vulns": [{"id": "GHSA-0000-0000-0000", "severity": "critical"}]}
        ]
        code, out = self.run_gate("--tree", self.write("t.json", tree), "--diff", str(FIX / "npm-diff.json"))
        self.assertEqual(code, 0, out)

    def test_an_empty_diff_passes(self):
        code, out = self.run_gate("--tree", str(FIX / "clean-tree.json"), "--diff", str(FIX / "empty-diff.json"))
        self.assertEqual(code, 0, out)

    def test_a_root_the_base_does_not_have_needs_a_maintainer(self):
        diff = fixture("npm-diff.json")
        diff["changed"], diff["summary"] = [], {"added": 2, "changed": 0, "removed": 0, "unchanged": 0}
        code, out = self.run_gate("--tree", str(FIX / "clean-tree.json"), "--diff", self.write("d.json", diff))
        self.assertEqual(code, 1)
        self.assertIn("new or moved dependency root (ui)", out)

    def test_more_than_fifty_added_needs_a_maintainer(self):
        diff = fixture("npm-diff.json")
        diff["added"] = [{"name": f"p{i}", "version": "1.0.0", "assessment": None} for i in range(51)]
        code, out = self.run_gate("--tree", str(FIX / "clean-tree.json"), "--diff", self.write("d.json", diff))
        self.assertEqual(code, 1)
        self.assertIn("51 packages added", out)

    def test_an_unreadable_or_unknown_diff_fails(self):
        for doc in ("{not json", json.dumps({"schema_version": 3})):
            code, _ = self.run_gate("--tree", str(FIX / "clean-tree.json"), "--diff", self.write("d.json", doc))
            self.assertEqual(code, 1, doc)
        code, _ = self.run_gate("--tree", str(FIX / "clean-tree.json"), "--diff", "/nonexistent/diff.json")
        self.assertEqual(code, 1)

    # --- push and schedule: block ----------------------------------------

    def test_without_a_diff_every_serious_advisory_fails(self):
        tree = fixture("npm-tree.json")
        tree["vulnerabilities"] = [
            {"name": "left-pad", "version": "1.3.0", "ecosystem": "npm",
             "vulns": [{"id": "GHSA-0000-0000-0000", "severity": "high"}]}
        ]
        code, out = self.run_gate("--tree", self.write("t.json", tree))
        self.assertEqual(code, 1)
        self.assertIn("left-pad@1.3.0", out)

    def test_fail_on_critical_lets_a_high_through(self):
        tree = fixture("npm-tree.json")
        tree["vulnerabilities"] = [
            {"name": "left-pad", "version": "1.3.0", "ecosystem": "npm",
             "vulns": [{"id": "GHSA-0000-0000-0000", "severity": "high"}]}
        ]
        code, out = self.run_gate("--tree", self.write("t.json", tree), "--fail-on", "critical")
        self.assertEqual(code, 0, out)

    def test_a_clean_tree_passes(self):
        code, out = self.run_gate("--tree", str(FIX / "clean-tree.json"))
        self.assertEqual(code, 0, out)
        self.assertIn("postmortem gate passed", out)

    # --- a lookup that did not complete ----------------------------------

    def test_a_failed_lookup_is_never_clean(self):
        # The recording: exit 0, an empty SARIF, the diagnostic in the JSON only.
        code, out = self.run_gate(
            "--tree", str(FIX / "npm-tree-failed.json"), "--sarif", str(FIX / "npm-tree-failed.sarif"),
            "--diff", str(FIX / "npm-diff.json"),
        )
        self.assertEqual(code, 1)
        self.assertIn("the vulnerability lookup did not complete", out)

    def test_a_rate_limit_says_so(self):
        tree = fixture("npm-tree-failed.json")
        tree["diagnostics"][0]["message"] = "vuln scan failed: mlab rate limit reached (try a token)"
        code, out = self.run_gate("--tree", self.write("t.json", tree))
        self.assertEqual(code, 1)
        self.assertIn("vuln.mlab.sh did not answer (rate limit)", out)

    def test_an_absent_or_unreadable_tree_fails(self):
        self.assertEqual(self.run_gate("--tree", "/nonexistent/tree.json")[0], 1)
        self.assertEqual(self.run_gate("--tree", self.write("t.json", "[]"))[0], 1)
        self.assertEqual(self.run_gate("--tree", self.write("t.json", "{oops"))[0], 1)

    def test_a_tree_without_a_package_count_fails(self):
        tree = fixture("clean-tree.json")
        del tree["stats"]
        code, out = self.run_gate("--tree", self.write("t.json", tree))
        self.assertEqual(code, 1)
        self.assertIn("no package count", out)

    # --- decision 50: the service's tier ---------------------------------

    def test_a_root_over_the_anonymous_tier_fails_without_a_token(self):
        tree = fixture("clean-tree.json")
        tree["stats"]["total"] = 513
        code, out = self.run_gate("--tree", self.write("t.json", tree))
        self.assertEqual(code, 1)
        self.assertIn("ui has 513 packages; an anonymous scan reads 512 — not scannable without VULN_MLAB_TOKEN", out)
        with mock.patch.dict(os.environ, {"VULN_MLAB_TOKEN": "x"}):
            self.assertEqual(self.run_gate("--tree", self.write("t.json", tree))[0], 0)
            tree["stats"]["total"] = 1025
            code, out = self.run_gate("--tree", self.write("t.json", tree))
            self.assertEqual(code, 1)
            self.assertIn("a token scan reads 1024", out)

    # --- the SARIF and MAL- -------------------------------------------------

    def test_a_mal_id_as_its_own_advisory_fails(self):
        code, out = self.run_gate("--tree", str(FIX / "npm-tree.json"), "--sarif", str(FIX / "npm-tree.sarif"),
                                  "--fail-on", "critical", "--diff", str(FIX / "empty-diff.json"))
        self.assertEqual(code, 1)
        self.assertIn("(MAL-2023-1000)", out)

    def test_a_mal_alias_of_a_ghsa_fails(self):
        # The rustdecimal recording: the GHSA, MAL-2022-1 and the RUSTSEC id
        # are three results.
        code, out = self.run_gate("--tree", str(FIX / "cargo-tree.json"), "--sarif", str(FIX / "cargo-tree.sarif"),
                                  "--fail-on", "critical")
        self.assertEqual(code, 1)
        self.assertIn("known-malicious package rustdecimal@1.23.1 (MAL-2022-1)", out)

    def test_a_mal_found_only_in_the_sarif_still_fails(self):
        tree = fixture("cargo-tree.json")
        tree["vulnerabilities"][0]["vulns"][1]["id"] = "GHSA-aaaa-bbbb-cccc"
        code, out = self.run_gate("--tree", self.write("t.json", tree), "--sarif", str(FIX / "cargo-tree.sarif"),
                                  "--fail-on", "critical")
        self.assertEqual(code, 1)
        self.assertIn("(MAL-2022-1)", out)

    def test_a_package_named_with_mal_in_it_does_not_block(self):
        tree = fixture("clean-tree.json")
        tree["vulnerabilities"] = [{"name": "MAL-2023-1000", "version": "1.0.0", "ecosystem": "npm",
                                    "vulns": [{"id": "GHSA-1111-2222-3333", "severity": "low"}]}]
        sarif = fixture("npm-tree.sarif")
        sarif["runs"][0]["results"] = [{
            "ruleId": gate.VULN_RULE, "level": "note",
            "message": {"text": "MAL-2023-1000@1.0.0: GHSA-1111-2222-3333 — MAL-2023-1000 in the name"},
            "properties": {"advisory": "GHSA-1111-2222-3333"},
        }]
        code, out = self.run_gate("--tree", self.write("t.json", tree), "--sarif", self.write("s.sarif", sarif))
        self.assertEqual(code, 0, out)

    def test_a_sarif_without_results_or_absent_fails(self):
        sarif = fixture("cargo-tree.sarif")
        del sarif["runs"][0]["results"]
        code, out = self.run_gate("--tree", str(FIX / "clean-tree.json"), "--sarif", self.write("s.sarif", sarif))
        self.assertEqual(code, 1)
        self.assertIn("no runs[].results", out)
        self.assertEqual(self.run_gate("--tree", str(FIX / "clean-tree.json"), "--sarif", "/nonexistent.sarif")[0], 1)
        self.assertEqual(self.run_gate("--tree", str(FIX / "clean-tree.json"), "--sarif", self.write("s.sarif", "{}"))[0], 1)

    def test_a_sarif_that_lost_its_lookup_disagrees_with_the_json(self):
        # The JSON run answered, the SARIF run did not: empty but well formed.
        code, out = self.run_gate("--tree", str(FIX / "npm-tree.json"), "--sarif", str(FIX / "npm-tree-failed.sarif"),
                                  "--diff", str(FIX / "empty-diff.json"))
        self.assertEqual(code, 1)
        self.assertIn("the SARIF carries 0 advisories and the JSON 7", out)

    # --- output ------------------------------------------------------------

    def test_a_hostile_package_name_is_escaped(self):
        tree = fixture("clean-tree.json")
        evil = "x\n::error::forged%"
        tree["vulnerabilities"] = [{"name": evil, "version": "1.0.0", "ecosystem": "npm",
                                    "vulns": [{"id": "GHSA-9999-9999-9999", "severity": "critical"}]}]
        code, out = self.run_gate("--tree", self.write("t.json", tree))
        self.assertEqual(code, 1)
        self.assertNotIn("\n::error::forged", out)
        self.assertIn("x%0A:%3Aerror:%3Aforged%25@1.0.0", out)
        for line in out.splitlines():
            self.assertTrue(line.startswith(("::error::", "::warning::")) or "::" not in line, line)

    def test_other_diagnostics_are_warnings(self):
        tree = fixture("clean-tree.json")
        tree["diagnostics"] = [{"kind": "something_else", "message": "note"}]
        code, out = self.run_gate("--tree", self.write("t.json", tree))
        self.assertEqual(code, 0)
        self.assertIn("::warning::postmortem diagnostic on ui: note", out)


if __name__ == "__main__":
    unittest.main()
