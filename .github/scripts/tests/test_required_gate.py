"""required_gate.py — the aggregate `<workflow> / required` job (RFC 0041 decisions 28, 41).

The `needs` shapes are GitHub's `toJSON(needs)`: `{job: {result, outputs}}`,
`result` one of success, failure, cancelled, skipped.
"""

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import required_gate as rg  # noqa: E402

TEST_GLOBS = ["crates/**", "**/Cargo.toml", "**/Cargo.lock", ".github/workflows/test.yaml"]


def gate(run="true", reason="", lockfile="false", result="success"):
    return {"result": result, "outputs": {"run": run, "reason": reason, "lockfile": lockfile}}


class Globs(unittest.TestCase):
    def test_double_star_slash_matches_the_root_too(self):
        self.assertTrue(rg.any_match(["Cargo.lock"], ["**/Cargo.lock"]))
        self.assertTrue(rg.any_match(["fuzz/Cargo.lock"], ["**/Cargo.lock"]))

    def test_trailing_double_star_crosses_directories(self):
        self.assertTrue(rg.any_match(["crates/core/src/lib.rs"], ["crates/**"]))

    def test_single_star_stays_in_one_segment(self):
        self.assertFalse(rg.any_match(["ui/src/a/b.ts"], ["ui/*.ts"]))
        self.assertTrue(rg.any_match(["ui/a.ts"], ["ui/*.ts"]))

    def test_dots_are_literal(self):
        self.assertFalse(rg.any_match([".github/workflows/testXyaml"], [".github/workflows/test.yaml"]))

    def test_a_ui_only_change_misses_the_rust_filter(self):
        self.assertFalse(rg.any_match(["ui/src/App.vue", "ui/pnpm-lock.yaml"], TEST_GLOBS))


class Changed(unittest.TestCase):
    def test_schedule_and_dispatch_are_full_runs(self):
        for ev in ("schedule", "workflow_dispatch"):
            self.assertTrue(rg.changed(ev, "", "", ["nothing/**"]))

    def test_a_new_branch_push_is_a_full_run(self):
        self.assertTrue(rg.changed("push", rg.ZERO_SHA, "abc", ["nothing/**"]))

    def test_an_unknown_base_is_a_full_run_not_a_skip(self):
        self.assertTrue(rg.changed("push", "f" * 40, "HEAD", ["nothing/**"]))

    def test_a_real_three_dot_diff(self):
        with tempfile.TemporaryDirectory() as d:
            def git(*a):
                return subprocess.run(["git", "-C", d, *a], check=True, capture_output=True, text=True).stdout.strip()

            git("init", "-q", "-b", "main")
            git("-c", "user.email=t@t", "-c", "user.name=t", "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "base")
            base = git("rev-parse", "HEAD")
            Path(d, "ui").mkdir()
            Path(d, "ui", "App.vue").write_text("x")
            git("add", ".")
            git("-c", "user.email=t@t", "-c", "user.name=t", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "ui")
            head = git("rev-parse", "HEAD")
            self.assertFalse(rg.changed("pull_request", base, head, TEST_GLOBS, cwd=d))
            self.assertTrue(rg.changed("pull_request", base, head, ["ui/**"], cwd=d))
            # A file moved out of a watched tree is still a change to that tree.
            git("mv", "ui/App.vue", "App.vue")
            git("-c", "user.email=t@t", "-c", "user.name=t", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "mv")
            self.assertTrue(rg.changed("pull_request", head, git("rev-parse", "HEAD"), ["ui/**"], cwd=d))
            Path(d, "ui", "café.ts").write_text("x")
            git("add", ".")
            git("-c", "user.email=t@t", "-c", "user.name=t", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "utf8")
            self.assertTrue(rg.changed("pull_request", head, git("rev-parse", "HEAD"), ["ui/*.ts"], cwd=d))


class Verdict(unittest.TestCase):
    def test_all_green_passes(self):
        ok, _ = rg.verdict({"gate": gate(), "lint": {"result": "success"}, "unit": {"result": "success"}})
        self.assertTrue(ok)

    def test_a_skipped_job_fails_when_the_suite_was_meant_to_run(self):
        ok, why = rg.verdict({"gate": gate(), "lint": {"result": "skipped"}})
        self.assertFalse(ok)
        self.assertIn("lint=skipped", why)

    def test_a_failure_or_cancel_fails(self):
        for r in ("failure", "cancelled"):
            self.assertFalse(rg.verdict({"gate": gate(), "unit": {"result": r}})[0])

    def test_skipped_by_the_filter_passes(self):
        ok, why = rg.verdict({"gate": gate(run="false", reason="filter"), "lint": {"result": "skipped"}})
        self.assertTrue(ok)
        self.assertIn("filter", why)

    def test_dependabot_skip_of_an_action_bump_passes(self):
        ok, _ = rg.verdict({"gate": gate(run="false", reason="dependabot"), "lint": {"result": "skipped"}})
        self.assertTrue(ok)

    def test_dependabot_skip_of_a_lockfile_bump_asks_for_full_test(self):
        ok, why = rg.verdict(
            {"gate": gate(run="false", reason="dependabot", lockfile="true"), "lint": {"result": "skipped"}}
        )
        self.assertFalse(ok)
        self.assertIn("full-test", why)

    def test_a_missing_run_output_fails_rather_than_reading_as_a_skip(self):
        for bad in ("", None, "True"):
            ok, why = rg.verdict({"gate": {"result": "success", "outputs": {"run": bad}}, "lint": {"result": "skipped"}})
            self.assertFalse(ok, bad)
            self.assertIn("run output", why)

    def test_every_required_job_waits_for_every_other_job(self):
        """A job added to a gated workflow and not to `required.needs` would never block."""
        import re

        # Jobs that run on a schedule or a push only, so a PR never sees them.
        not_waited = {"test.yaml": {"heavy-authz-live"}, "postmortem.yaml": {"issue"}}
        wf = Path(__file__).resolve().parents[2] / "workflows"
        seen = 0
        for f in sorted(wf.glob("*.yaml")):
            text = f.read_text()
            if f"name: {f.name} / required" not in text:
                continue
            seen += 1
            body = text.split("\njobs:\n", 1)[1]
            jobs = set(re.findall(r"^  ([a-z][a-z0-9_-]*):\n", body, re.M)) - {"required"}
            req = body.split("\n  required:\n", 1)[1]
            needs = set(re.search(r"^    needs: \[([^\]]*)\]", req, re.M).group(1).replace(" ", "").split(","))
            self.assertEqual(needs, jobs - not_waited.get(f.name, set()), f.name)
        self.assertGreaterEqual(seen, 12)

    def test_a_failed_gate_fails(self):
        self.assertFalse(rg.verdict({"gate": gate(result="failure")})[0])

    def test_without_a_gate_every_job_must_succeed(self):
        self.assertTrue(rg.verdict({"rust": {"result": "success"}, "ui": {"result": "success"}})[0])
        self.assertFalse(rg.verdict({"rust": {"result": "success"}, "ui": {"result": "skipped"}})[0])

    def test_main_exit_codes(self):
        import json

        self.assertEqual(rg.main(["verdict", json.dumps({"gate": gate()})]), 0)
        self.assertEqual(rg.main(["verdict", json.dumps({"gate": gate(result="cancelled")})]), 1)
        self.assertEqual(rg.main(["verdict", "not json"]), 1)


if __name__ == "__main__":
    unittest.main()
