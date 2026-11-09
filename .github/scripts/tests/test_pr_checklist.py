"""pr_checklist.py's gate-input warning (RFC 0041 §6.5, decisions 30 and 47).

`pr-checklist.yaml` runs on `pull_request_target` and never checks out the
head: the changed paths come from the pulls/files API. `pr-296-files.json`
is that API's answer for batlehub/batlehub#296, recorded on 2026-11-08; each
case swaps one file into it.
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
import pr_checklist  # noqa: E402

RECORDED = json.loads((Path(__file__).resolve().parent / "fixtures" / "pr-296-files.json").read_text())

# One concrete path per entry of GATE_INPUTS.
GATE_INPUT_PATHS = {
    ".github/workflows/*": ".github/workflows/new-workflow.yaml",
    ".github/scripts/*": ".github/scripts/postmortem_new_risk.py",
    ".github/actions/*": ".github/actions/postmortem-gate/action.yml",
    ".github/coverage-floors.txt": ".github/coverage-floors.txt",
    ".github/CODEOWNERS": ".github/CODEOWNERS",
    ".github/mise-cve-budget.json": ".github/mise-cve-budget.json",
    "mise.toml": "mise.toml",
    "mise.lock": "mise.lock",
    ".tasks/*": ".tasks/security.yaml",
    "ui/vite.config.ts": "ui/vite.config.ts",
    ".cargo/mutants.toml": ".cargo/mutants.toml",
    "deny.toml": "deny.toml",
    ".cargo/audit.toml": ".cargo/audit.toml",
    "gitleaks.toml": "gitleaks.toml",
    ".gitleaksignore": ".gitleaksignore",
    ".semgrep/*": ".semgrep/rules.yaml",
    ".semgrep.yml": ".semgrep.yml",
    ".semgrepignore": ".semgrepignore",
    ".trivyignore*": ".trivyignore.yaml",
    "vex/*": "vex/batlehub.openvex.json",
    ".forgejo/workflows/*": ".forgejo/workflows/build.yaml",
    ".forgejo/CODEOWNERS": ".forgejo/CODEOWNERS",
    "ui/pnpm-workspace.yaml": "ui/pnpm-workspace.yaml",
    "docs/pnpm-workspace.yaml": "docs/pnpm-workspace.yaml",
}


def entry(filename, patch="@@ -1 +1 @@\n-a\n+b", **extra):
    e = copy.deepcopy(RECORDED[0])
    e.update(filename=filename, patch=patch, **extra)
    return e


class GateInputs(unittest.TestCase):
    def render(self, files):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as fh:
            json.dump(files, fh)
        out = io.StringIO()
        with mock.patch.object(sys, "argv", ["pr_checklist.py", "--api-files", fh.name]), \
                contextlib.redirect_stdout(out):
            self.assertEqual(pr_checklist.main(), 0)
        Path(fh.name).unlink()
        return out.getvalue()

    def test_every_gate_input_has_a_case(self):
        self.assertEqual(set(GATE_INPUT_PATHS) | {pr_checklist.INLINE_NOSEMGREP}, set(pr_checklist.GATE_INPUTS))

    def test_each_gate_input_is_named(self):
        for glob, path in GATE_INPUT_PATHS.items():
            with self.subTest(glob=glob):
                out = self.render([entry("README.md"), entry(path)])
                self.assertIn("### A gate input changed", out)
                self.assertIn(f"  - `{path}`", out)
                self.assertNotIn("  - `README.md`", out)

    def test_the_recorded_answer_names_its_gate_inputs(self):
        out = self.render(RECORDED)
        for path in (".trivyignore.yaml", "mise.lock", "vex/batlehub.openvex.json"):
            self.assertIn(f"  - `{path}`", out)
        self.assertNotIn("  - `CHANGELOG.md`", out)

    def test_an_added_nosemgrep_comment_is_a_gate_input(self):
        out = self.render([entry("crates/core/src/lib.rs", "@@ -1 +1,2 @@\n a\n+let x = y; // nosemgrep: rule")])
        self.assertIn(f"  - `{pr_checklist.INLINE_NOSEMGREP}`", out)

    def test_a_removed_nosemgrep_comment_is_not(self):
        out = self.render([entry("crates/core/src/lib.rs", "@@ -1,2 +1 @@\n a\n-let x = y; // nosemgrep: rule")])
        self.assertNotIn("A gate input changed", out)

    def test_a_file_renamed_away_from_a_gate_input_counts(self):
        out = self.render([entry("old-ci/build.yaml", previous_filename=".github/workflows/build.yaml")])
        self.assertIn("  - `.github/workflows/build.yaml`", out)

    def test_paged_answers_are_flattened(self):
        out = self.render([[entry("README.md")], [entry("deny.toml")]])
        self.assertIn("  - `deny.toml`", out)

    def test_no_gate_input_no_section(self):
        out = self.render([entry("README.md"), entry("docs/guide/index.md")])
        self.assertNotIn("A gate input changed", out)


if __name__ == "__main__":
    unittest.main()
