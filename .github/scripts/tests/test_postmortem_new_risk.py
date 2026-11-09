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



class Reputation(unittest.TestCase):
    """Phase 4: `diff --online` and the approval of decision 46.

    `diff-online.json` was written by postmortem 2.9.2 on 2026-11-08 over a
    base pinning tslib 2.8.1, left-pad 1.3.0 and typescript 7.0.2 and a head
    moving tslib to 2.9.1 (`new-publisher`, as M9 saw on PR #270) and
    typescript to the 3-hour-old 7.1.0-dev.20261108.1 (`fresh-release`), and
    adding esbuild 0.25.0 (an install script `diff` does not report;
    `scripts-head.json` does) and lodahs (`typosquat of lodash`).
    `diff-online-stats-failed.json` is the same diff with GitHub refusing the
    token: exit 0, and `stats-failed` on each package as the only trace
    (§17 M20). The `gh-*.json` files are api.github.com's answers for PR #243.
    """

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        env = mock.patch.dict(os.environ, {"GITHUB_REPOSITORY": "batlehub/batlehub", "GITHUB_TOKEN": "t"})
        env.start()
        self.addCleanup(env.stop)
        os.environ.pop("VULN_MLAB_TOKEN", None)
        self.pull = fixture("gh-pull-243.json")
        self.head = self.pull["head"]["sha"]
        self.reviews = []
        self.roles = {}
        self.calls = []
        api = mock.patch.object(gate, "api_get", side_effect=self.fake_api)
        api.start()
        self.addCleanup(api.stop)

    def review(self, login, state="APPROVED", commit=None):
        r = fixture("gh-reviews-243.json")[0]
        r = {**r, "user": {**r["user"], "login": login}, "state": state, "commit_id": commit or self.head}
        self.reviews.append(r)

    def fake_api(self, url, token, timeout):
        self.calls.append(url)
        path = url.split("/repos/batlehub/batlehub/", 1)[-1]
        if path == "pulls/243":
            return self.pull
        if path.startswith("pulls/243/reviews"):
            page = int(path.rsplit("page=", 1)[1])
            return self.reviews[(page - 1) * 100 : page * 100]
        if path.startswith("collaborators/"):
            login = path.split("/")[1]
            base = fixture("gh-permission-admin.json" if self.roles.get(login) == "admin" else "gh-permission-read.json")
            return {**base, "role_name": self.roles.get(login, "read")}
        if url.endswith("/rate_limit"):
            return {"resources": {"core": {"remaining": 0, "reset": 1_800_000_000}}}
        raise AssertionError(f"unexpected call {url}")

    def write(self, name, doc):
        path = Path(self.tmp.name) / name
        path.write_text(doc if isinstance(doc, str) else json.dumps(doc))
        return str(path)

    def online(self, doc=None, head_scripts=None, *extra):
        out = io.StringIO()
        args = [
            "--root", "ui", "--tree", str(FIX / "clean-tree.json"),
            "--online-diff", self.write("o.json", doc) if doc is not None else str(FIX / "diff-online.json"),
            "--scripts-base", str(FIX / "scripts-base.json"),
            "--scripts-head", self.write("h.json", head_scripts) if head_scripts is not None else str(FIX / "scripts-head.json"),
            *extra,
        ]
        with contextlib.redirect_stdout(out):
            code = gate.main(args)
        return code, out.getvalue()

    def only(self, kind, pkg):
        doc = fixture("diff-online.json")
        doc["added"] = [p for p in doc["added"] if kind == "added" and p["name"] == pkg]
        doc["changed"] = [p for p in doc["changed"] if kind == "changed" and p["name"] == pkg]
        return doc

    def no_scripts(self):
        return {**fixture("scripts-head.json"), "packages": []}

    def test_the_recorded_change_blocks_on_each_rule(self):
        code, out = self.online()
        self.assertEqual(code, 1)
        self.assertIn("lodahs@0.0.1 — typosquat of lodash (transposed)", out)
        self.assertIn("esbuild@0.25.0 — install script", out)
        self.assertIn("tslib@2.9.1 — new-publisher", out)
        self.assertIn("typescript@7.1.0-dev.20261108.1 — fresh-release (3h old)", out)
        self.assertIn("tslib@2.9.1: reported, not blocking — dormant-release (728d gap)", out)
        self.assertIn("lodahs@0.0.1: reported, not blocking — no-repository", out)

    def test_signals_outside_the_blocking_set_are_reported_only(self):
        doc = self.only("added", "lodahs")
        doc["added"][0]["assessment"]["signals"] = ["no-repository", "newborn-package (21d old)", "low-stars (3)"]
        code, out = self.online(doc, self.no_scripts())
        self.assertEqual(code, 0, out)
        self.assertIn("reported, not blocking — no-repository, newborn-package (21d old), low-stars (3)", out)

    def test_a_newborn_package_under_seven_days_blocks(self):
        doc = self.only("added", "lodahs")
        doc["added"][0]["assessment"]["signals"] = ["newborn-package (3d old)"]
        code, out = self.online(doc, self.no_scripts())
        self.assertEqual(code, 1)
        self.assertIn("lodahs@0.0.1 — newborn-package (3d old)", out)

    def test_an_age_it_cannot_read_counts_as_young(self):
        doc = self.only("changed", "typescript")
        doc["changed"][0]["assessment"]["signals"] = ["fresh-release (soon)"]
        self.assertEqual(self.online(doc, self.no_scripts())[0], 1)

    def test_a_version_change_without_a_takeover_signal_passes(self):
        doc = self.only("changed", "typescript")
        doc["changed"][0]["assessment"]["signals"] = ["fresh-release (9d old)"]
        code, out = self.online(doc, self.no_scripts())
        self.assertEqual(code, 0, out)

    def test_a_version_change_that_gains_an_install_script_blocks(self):
        doc = self.only("changed", "tslib")
        doc["changed"][0]["assessment"]["signals"] = []
        head = {**fixture("scripts-head.json"), "packages": [{**fixture("scripts-head.json")["packages"][0], "name": "tslib"}]}
        code, out = self.online(doc, head)
        self.assertEqual(code, 1)
        self.assertIn("tslib@2.9.1 — install script the old version did not have", out)

    def test_cargo_build_scripts_are_not_install_scripts(self):
        doc = self.only("added", "lodahs")
        doc["added"][0]["assessment"]["signals"] = []
        head = {**fixture("scripts-head.json"), "packages": [{**fixture("scripts-head.json")["packages"][0], "name": "lodahs", "ecosystem": "rust"}]}
        self.assertEqual(self.online(doc, head)[0], 0)

    def test_stats_failed_fails_closed_with_the_reset_time_and_no_approval_lifts_it(self):
        self.roles["maint"] = "maintain"
        self.review("maint")
        code, out = self.online(fixture("diff-online-stats-failed.json"), None, "--pr", "243")
        self.assertEqual(code, 1)
        self.assertIn("the reputation lookup did not complete for 3 package(s)", out)
        self.assertIn("rate limit resets at", out)
        self.assertNotIn("::error::approved", out)

    def test_an_approval_of_the_head_by_a_maintainer_turns_it_report_only(self):
        self.roles["maint"] = "maintain"
        self.review("maint")
        code, out = self.online(None, None, "--pr", "243")
        self.assertEqual(code, 0, out)
        self.assertIn("::notice::approved on this head by maint", out)
        self.assertIn("::warning::lodahs@0.0.1 — typosquat of lodash (transposed)", out)

    def test_an_approval_of_an_earlier_head_does_not(self):
        self.roles["maint"] = "admin"
        self.review("maint", commit="0" * 40)
        code, out = self.online(None, None, "--pr", "243")
        self.assertEqual(code, 1)
        self.assertIn(f"no approving review of head {self.head}", out)

    def test_the_authors_own_approval_or_a_triage_account_does_not(self):
        self.roles[self.pull["user"]["login"]] = "admin"
        self.review(self.pull["user"]["login"])
        self.roles["helper"] = "triage"
        self.review("helper")
        self.review("other", state="COMMENTED")
        self.assertEqual(self.online(None, None, "--pr", "243")[0], 1)

    def test_a_review_list_longer_than_a_page_is_read_to_its_end(self):
        for i in range(150):
            self.review(f"drive-by-{i}", state="COMMENTED")
        self.roles["maint"] = "maintain"
        self.review("maint")
        self.assertEqual(self.online(None, None, "--pr", "243")[0], 0)
        self.assertTrue(any("page=2" in c for c in self.calls))

    def test_an_api_error_or_a_timeout_is_red(self):
        for exc in (gate.urllib.error.URLError("boom"), TimeoutError("slow")):
            with self.subTest(exc=exc), mock.patch.object(gate, "api_get", side_effect=exc):
                code, out = self.online(None, None, "--pr", "243")
                self.assertEqual(code, 1)
                self.assertIn("the reviews API failed", out)

    def test_a_new_root_is_lifted_by_an_approval_but_a_mal_package_never_is(self):
        self.roles["maint"] = "maintain"
        self.review("maint")
        diff = fixture("empty-diff.json")
        diff["added"] = [{"name": f"p{i}", "version": "1.0.0"} for i in range(3)]
        diff["summary"] = {"added": 3, "changed": 0, "removed": 0, "unchanged": 0}
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = gate.main(["--root", "ui", "--tree", str(FIX / "clean-tree.json"), "--diff", self.write("d.json", diff), "--pr", "243"])
        self.assertEqual(code, 0, out.getvalue())
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = gate.main(["--root", "ui", "--tree", str(FIX / "npm-tree.json"), "--sarif", str(FIX / "npm-tree.sarif"),
                              "--diff", str(FIX / "empty-diff.json"), "--pr", "243"])
        self.assertEqual(code, 1)
        self.assertIn("known-malicious package", out.getvalue())

    def test_resolve_failed_fails_closed_like_stats_failed(self):
        # postmortem 2.9.2 behind a dead proxy: every package `resolve-failed`, exit 0.
        doc = fixture("diff-online.json")
        for kind in ("added", "changed"):
            for pkg in doc.get(kind) or []:
                pkg["assessment"] = {"risk": 0, "signals": ["resolve-failed"]}
        self.roles["maint"] = "maintain"
        self.review("maint")
        code, out = self.online(doc, None, "--pr", "243")
        self.assertEqual(code, 1)
        self.assertIn("the reputation lookup did not complete", out)
        self.assertNotIn("reported, not blocking — resolve-failed", out)

    def test_an_approval_replaced_by_a_change_request_lifts_nothing(self):
        self.roles["maint"] = "maintain"
        self.review("maint")
        self.review("maint", state="CHANGES_REQUESTED")
        self.assertEqual(self.online(None, None, "--pr", "243")[0], 1)

    def test_a_new_root_still_judges_its_advisories(self):
        diff = fixture("empty-diff.json")
        diff["added"] = [{"name": "lodash", "version": "4.17.20"}]
        diff["summary"] = {"added": 1, "changed": 0, "removed": 0, "unchanged": 0}
        vulns = {("lodash", "4.17.20"): [{"id": "GHSA-35jh-r3h4-6jhm", "severity": "high"}]}
        errors, liftable = [], []
        with contextlib.redirect_stdout(io.StringIO()):
            gate.check_diff(diff, vulns, "ui", gate.RANK["high"], errors, liftable)
        self.assertTrue(liftable)
        self.assertTrue(any("lodash" in e for e in errors), errors)

    def test_online_needs_both_script_inventories(self):
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            gate.main(["--root", "ui", "--tree", str(FIX / "clean-tree.json"), "--online-diff", str(FIX / "diff-online.json")])

    def test_a_package_name_cannot_issue_a_workflow_command(self):
        doc = self.only("added", "lodahs")
        doc["added"][0]["name"] = "evil\n::error::pwned"
        code, out = self.online(doc, self.no_scripts())
        self.assertNotIn("\n::error::pwned", out)


if __name__ == "__main__":
    unittest.main()
