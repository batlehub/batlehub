"""mise_repo_tools.sh, driven as `mise-scan.yaml` runs it: `bash` in a subprocess.

`fixtures/mise_repo_tools/mise-ls-current.json` is mise 2026.11.3's
`mise ls --current --json` in this repository with the global config pointed
at an empty `.toml`, recorded on 2026-11-08, and `EXPECTED` is what the script
printed over it that day. `mise` is a stub on `PATH` that answers with a
listing and, like the real 2026.11 binary, refuses a global config file that
is not a `.toml` ("unknown config file type").
"""

import json
import os
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE.parent / "mise_repo_tools.sh"
RECORDED = HERE / "fixtures" / "mise_repo_tools" / "mise-ls-current.json"
EXPECTED = (
    "aqua-embark-studios-cargo-deny,cargo-cargo-audit,cargo-cargo-cyclonedx,cargo-cargo-sweep,claude,cocogitto,"
    "github-batleforc-weebo-skillsagent,github-mlab-sh-mlab-cli,github-mlab-sh-postmortem,github-rustfs-cli,gitleaks,"
    "go,go-github-com-norwoodj-helm-docs-cmd-helm-docs,helm,k6,node,pnpm,python,rust,semgrep,sonar-scanner-cli,syft,"
    "task,trivy,uv"
)


class MiseRepoTools(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.seen = self.tmp / "seen.json"

    def run_script(self, listing: str, exit_code: int = 0) -> subprocess.CompletedProcess:
        bin_dir = self.tmp / "bin"
        bin_dir.mkdir(exist_ok=True)
        stub = bin_dir / "mise"
        stub.write_text(
            f"#!{sys.executable}\n"
            + textwrap.dedent(f"""\
                import json, os, sys
                cfg = os.environ.get("MISE_GLOBAL_CONFIG_FILE", "")
                with open({str(self.seen)!r}, "w") as fh:
                    json.dump({{"argv": sys.argv[1:], "cfg": cfg,
                               "exists": os.path.isfile(cfg),
                               "size": os.path.getsize(cfg) if os.path.isfile(cfg) else None}}, fh)
                if not cfg.endswith(".toml"):
                    sys.exit("mise ERROR unknown config file type: " + cfg)
                if {exit_code}:
                    sys.exit("mise ERROR simulated failure")
                sys.stdout.write({listing!r})
            """)
        )
        stub.chmod(0o755)
        env = {**os.environ, "PATH": f"{bin_dir}{os.pathsep}{os.environ.get('PATH', '')}"}
        return subprocess.run(["bash", str(SCRIPT)], capture_output=True, text=True, env=env, timeout=30)

    def test_recorded_listing_gives_the_recorded_line(self):
        result = self.run_script(RECORDED.read_text())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, EXPECTED + "\n")

    def test_global_config_is_an_empty_toml_removed_afterwards(self):
        self.run_script(RECORDED.read_text())
        seen = json.loads(self.seen.read_text())
        self.assertEqual(seen["argv"], ["ls", "--current", "--json"])
        self.assertTrue(seen["cfg"].endswith(".toml"), seen["cfg"])
        self.assertEqual((seen["exists"], seen["size"]), (True, 0))
        self.assertFalse(Path(seen["cfg"]).exists(), "the trap must remove the temporary config")

    def test_mise_failing_fails_the_script(self):
        result = self.run_script("{}", exit_code=1)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn("simulated failure", result.stderr)

    def test_output_that_is_not_json_fails_the_script(self):
        result = self.run_script("mise WARN something\n")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")

    def test_versions_collapse_and_entries_without_a_path_are_skipped(self):
        root = "/home/runner/.local/share/mise/installs"
        listing = {
            "node": [{"version": "24.1.0", "install_path": f"{root}/node/24.1.0"}, {"version": "22.9.0", "install_path": f"{root}/node/22.9.0"}],
            "cargo:cargo-audit": [{"version": "0.22.2", "install_path": f"{root}/cargo-cargo-audit/0.22.2"}],
            "missing": [{"version": "1.0.0", "installed": False}],
            "empty": [],
        }
        result = self.run_script(json.dumps(listing))
        self.assertEqual((result.returncode, result.stdout), (0, "cargo-cargo-audit,node\n"))

    def test_no_tools_prints_an_empty_line(self):
        result = self.run_script("{}")
        self.assertEqual((result.returncode, result.stdout), (0, "\n"))


if __name__ == "__main__":
    unittest.main()
