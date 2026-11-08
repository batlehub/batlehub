"""mise_cve_report.py's budget verdict and its fix column (RFC 0041 §6.3).

`trivy-installs.json` is Trivy 0.76.1's `rootfs --scanners vuln --format json`
over three real mise install directories, recorded on 2026-11-08: helm-docs
1.14.2, lazydocker 0.25.2 and k9s 0.51.0. Each was scanned on its own and its
`Target` prefixed with the tool's directory, which is the spelling a scan of
the whole `installs/` root gives (`helm-docs/1.14.2/helm-docs`). The findings
were cut to 11 and the `Packages` list dropped; the script reads neither the
rest nor that list.

Fixable HIGH/CRITICAL per tool: helm-docs 4, lazydocker 2 (plus one HIGH with
no fixed version, as recorded), k9s 2 — so 8 in all, 6 without k9s.

`mise` is a stub on `PATH` answering in the shape `mise ls --current --json`
recorded in `fixtures/mise_repo_tools/`; no case asks a forge anything.
"""

import contextlib
import io
import json
import os
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import mise_cve_report  # noqa: E402

TRIVY = Path(__file__).resolve().parent / "fixtures" / "mise_cve_report" / "trivy-installs.json"


class MiseCveReport(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.root = self.tmp / "installs"
        for tool in ("helm-docs", "lazydocker", "k9s"):
            (self.root / tool).mkdir(parents=True)
        self.budget_file = self.tmp / "mise-cve-budget.json"
        self.md = self.tmp / "report.md"

    def budget(self, n: int) -> None:
        self.budget_file.write_text(json.dumps({"max_fixable_high_critical": n}))

    def run_main(self, *argv: str, upstream: bool = False) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        args = [
            "mise_cve_report.py",
            "--trivy-json", str(TRIVY),
            "--root", str(self.root),
            "--out", str(self.md),
            "--budget-file", str(self.budget_file),
            *([] if upstream else ["--no-upstream-check"]),
            *argv,
        ]  # fmt: skip
        with mock.patch.object(sys, "argv", args), contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = mise_cve_report.main()
        return code, out.getvalue(), err.getvalue()

    def fake_mise(self, listing: dict, latest: dict) -> dict:
        """A `mise` on PATH answering `ls --current --json` and `latest <spec>`."""
        bin_dir = self.tmp / "bin"
        bin_dir.mkdir()
        script = bin_dir / "mise"
        script.write_text(
            f"#!{sys.executable}\n"
            + textwrap.dedent(f"""\
                import json, os, sys
                args = sys.argv[1:]
                # Real mise refuses a global config without a config extension
                # ("unknown config file type: /dev/null").
                if not os.environ.get("MISE_GLOBAL_CONFIG_FILE", "").endswith(".toml"):
                    sys.exit(1)
                if args == ["ls", "--current", "--json"]:
                    print(json.dumps({listing!r}))
                elif args[0] == "latest" and args[1] in {latest!r}:
                    print({latest!r}[args[1]])
                else:
                    sys.exit(1)
            """)
        )
        script.chmod(0o755)
        return {"PATH": f"{bin_dir}{os.pathsep}{os.environ.get('PATH', '')}"}

    def entry(self, tool: str, version: str, requested: str) -> list[dict]:
        return [{
            "version": version,
            "requested_version": requested,
            "install_path": f"{self.root}/{tool}/{version}",
            "source": {"type": "mise.toml", "path": "/projects/proxy-cache/mise.toml"},
            "installed": True,
            "active": True,
        }]  # fmt: skip

    # ── the budget verdict ───────────────────────────────────────────────────

    def test_no_budget_file_reports_and_cannot_fail(self):
        code, out, _ = self.run_main()
        self.assertEqual(code, 0)
        self.assertIn("⚪ **8 fixable HIGH/CRITICAL** — no budget recorded yet, so this run reports and cannot fail.", out)
        self.assertIn("10 finding(s) in total — Critical 2 · High 6 · Medium 1 · Low 1.", out)
        self.assertNotIn("::error::", out)
        self.assertEqual(self.md.read_text(), out.removesuffix("\n"))

    def test_under_and_at_budget_passes(self):
        for n in (8, 136):
            with self.subTest(budget=n):
                self.budget(n)
                code, out, _ = self.run_main()
                self.assertEqual(code, 0)
                self.assertIn(f"🟢 **8 fixable HIGH/CRITICAL**, within the budget of **{n}**", out)

    def test_over_budget_fails_with_the_annotation(self):
        self.budget(7)
        code, out, _ = self.run_main()
        self.assertEqual(code, 1)
        self.assertIn("🔴 **8 fixable HIGH/CRITICAL**, over the budget of **7**", out)
        self.assertIn(
            "::error::the development toolchain carries 8 fixable HIGH/CRITICAL findings, over its budget of 7",
            out.splitlines()[-1],
        )

    def test_budget_flag_overrides_the_file(self):
        self.budget(100)
        code, out, _ = self.run_main("--budget", "7")
        self.assertEqual(code, 1)
        self.assertIn("over the budget of **7**", out)
        code, _, _ = self.run_main("--budget", "8")
        self.assertEqual(code, 0)

    def test_budget_file_without_the_key_is_a_budget_of_zero(self):
        self.budget_file.write_text("{}")
        code, out, _ = self.run_main()
        self.assertEqual(code, 1)
        self.assertIn("over the budget of **0**", out)

    def test_only_tools_narrows_the_count_and_says_so(self):
        self.budget(6)
        code, out, _ = self.run_main("--only-tools", "helm-docs, lazydocker")
        self.assertEqual(code, 0)
        self.assertEqual(
            out.splitlines()[0],
            "scope: 2 fixable HIGH/CRITICAL in 1 install director(ies) outside this repository's mise.toml, not counted",
        )
        self.assertIn("within the budget of **6**", out)
        self.assertIn("*2 fixable HIGH/CRITICAL in 1 install director(ies) this repository does not ask for are **not** counted — `k9s` (2).", out)
        self.assertNotIn("| `k9s` |", out)

    def test_only_tools_keeps_what_is_not_an_install_directory(self):
        # k9s's directory does not exist under --root, so it cannot be attributed and is counted.
        (self.root / "k9s").rmdir()
        code, out, _ = self.run_main("--only-tools", "helm-docs,lazydocker")
        self.assertEqual(code, 0)
        self.assertIn("**8 fixable HIGH/CRITICAL**", out)
        self.assertNotIn("scope:", out)

    def test_include_unfixed_counts_the_unfixed_high(self):
        self.budget(8)
        code, out, _ = self.run_main("--include-unfixed")
        self.assertEqual(code, 1)
        self.assertIn("🔴 **9 fixable HIGH/CRITICAL**, over the budget of **8**", out)
        self.assertIn("| `lazydocker` | CVE-2026-41567 | HIGH | github.com/docker/docker | v28.5.2+incompatible | — |", out)

    def test_write_budget_records_the_current_count(self):
        code, out, _ = self.run_main("--write-budget")
        self.assertEqual(code, 0)
        self.assertIn(f"recorded budget: 8 → {self.budget_file}", out)
        recorded = json.loads(self.budget_file.read_text())
        self.assertEqual(recorded["max_fixable_high_critical"], 8)
        self.assertEqual(recorded["recorded_totals"], {"CRITICAL": 2, "HIGH": 6, "MEDIUM": 1, "LOW": 1})

    def test_write_budget_raising_it_is_judged_against_the_new_number(self):
        self.budget(7)
        code, out, _ = self.run_main("--write-budget")
        self.assertEqual(code, 0)
        self.assertIn("within the budget of **8**", out)
        self.assertNotIn("::error::", out)

    def test_json_out(self):
        self.budget(7)
        json_out = self.tmp / "report.json"
        code, _, _ = self.run_main("--json-out", str(json_out))
        self.assertEqual(code, 1)
        self.assertEqual(
            json.loads(json_out.read_text()),
            {
                "totals": {"CRITICAL": 2, "HIGH": 6, "MEDIUM": 1, "LOW": 1},
                "fixable_high_critical": 8,
                "budget": 7,
                "over_budget": True,
                "per_tool": {"helm-docs": 6, "lazydocker": 2, "k9s": 2},
            },
        )

    def test_empty_report(self):
        empty = self.tmp / "empty.json"
        empty.write_text(json.dumps({"SchemaVersion": 2, "ArtifactName": str(self.root), "Results": None}))
        self.budget(0)
        with mock.patch.object(sys, "argv", ["x", "--trivy-json", str(empty), "--out", str(self.md), "--budget-file", str(self.budget_file), "--no-upstream-check"]):
            with contextlib.redirect_stdout(io.StringIO()) as out:
                code = mise_cve_report.main()
        self.assertEqual(code, 0)
        self.assertIn("🟢 **0 fixable HIGH/CRITICAL**, within the budget of **0**", out.getvalue())
        self.assertIn("0 finding(s) in total — none.", out.getvalue())
        self.assertIn("No findings. 🎉", out.getvalue())

    # ── the fix column ───────────────────────────────────────────────────────

    def test_without_mise_the_fix_column_says_it_could_not_judge(self):
        with mock.patch.object(mise_cve_report.shutil, "which", return_value=None):
            code, out, _ = self.run_main(upstream=True)
        self.assertEqual(code, 0)
        self.assertEqual(out.splitlines()[0], "upstream check: mise could not be asked — the fix column will not judge releases")
        self.assertIn("| `helm-docs` | CRITICAL | 6 | golang.org/x/crypto, stdlib | fix upstream, tool release unknown |", out)

    def test_upstream_answers_split_reachable_from_stuck(self):
        listing = {"helm-docs": self.entry("helm-docs", "1.14.2", "1.14"), "lazydocker": self.entry("lazydocker", "0.25.2", "latest")}
        env = self.fake_mise(listing, {"helm-docs@1.14": "1.14.2", "lazydocker": "0.26.0"})
        with mock.patch.dict(os.environ, env):
            code, out, _ = self.run_main(upstream=True)
        self.assertEqual(code, 0)
        self.assertNotIn("upstream check:", out)
        self.assertIn("| `helm-docs` | CRITICAL | 6 | golang.org/x/crypto, stdlib | no newer release (1.14.2) |", out)
        self.assertIn("| `lazydocker` | HIGH | 2 | github.com/docker/cli, golang.org/x/text | bump to 0.26.0 |", out)
        self.assertIn("| `k9s` | HIGH | 2 | github.com/containerd/containerd, github.com/containerd/containerd/v2 | fix upstream, tool release unknown |", out)
        self.assertIn("**2 of 8 are reachable by a version bump.** The other 6 are in tools already at their newest release — `helm-docs` (4), `k9s` (2)", out)

    def test_upstream_all_reachable(self):
        env = self.fake_mise({"lazydocker": self.entry("lazydocker", "0.25.2", "latest")}, {"lazydocker": "0.26.0"})
        with mock.patch.dict(os.environ, env):
            code, out, _ = self.run_main("--only-tools", "lazydocker", upstream=True)
        self.assertEqual(code, 0)
        self.assertIn("**All 2 are reachable by a version bump**", out)

    def test_tool_of(self):
        self.assertEqual(mise_cve_report.tool_of("helm-docs/1.14.2/helm-docs", "/r/installs"), "helm-docs")
        self.assertEqual(mise_cve_report.tool_of("/r/installs/node/24.1.0/lib/x", "/r/installs"), "node")
        self.assertEqual(mise_cve_report.tool_of("/home/u/.local/share/mise/installs/go/1.25/x", ""), "go")
        self.assertEqual(mise_cve_report.tool_of("Node.js", "/r/installs"), "Node.js")


if __name__ == "__main__":
    unittest.main()
