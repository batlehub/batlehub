"""sonar_quality_gate.py's verdict, with SonarCloud replaced by recorded answers.

`project_status-error.json` and `project_status-ok.json` are
api/qualitygates/project_status as sonarcloud.io answered it on 2026-11-08 for
`batleforc_batlehub` (main, and pull request 296). The `issues/search` and
`hotspots/search` totals and the 404 body are recorded the same day.
api/ce/task needs a token, so its answer is written from its documented shape
(`{"task": {"status", "analysisId", …}}`). `NONE` and `WARN` are the two other
values the endpoint documents; WARN is deprecated and no longer emitted by
SonarCloud, so a recorded one does not exist.

No case touches the network: `urllib.request.urlopen` is replaced by `Fake`.
"""

import contextlib
import email.message
import io
import json
import os
import sys
import tempfile
import unittest
import urllib.error
import urllib.parse
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import sonar_quality_gate  # noqa: E402

FIXTURES = Path(__file__).resolve().parent / "fixtures" / "sonar_quality_gate"
SERVER = "https://sonarcloud.io"
CE_TASK = f"{SERVER}/api/ce/task?id=AZpBd5yUcRpLQ0Wq9X1a"
REPORT_TASK = f"""organization=batleforc
projectKey=batleforc_batlehub
serverUrl={SERVER}
serverVersion=8.0.0.79533
dashboardUrl={SERVER}/dashboard?id=batleforc_batlehub&pullRequest=296
ceTaskId=AZpBd5yUcRpLQ0Wq9X1a
ceTaskUrl={CE_TASK}
"""
ERROR_STATUS = json.loads((FIXTURES / "project_status-error.json").read_text())
OK_STATUS = json.loads((FIXTURES / "project_status-ok.json").read_text())
NO_ISSUES = {"total": 0, "p": 1, "ps": 1, "paging": {"pageIndex": 1, "pageSize": 1, "total": 0}, "issues": []}
NO_HOTSPOTS = {"paging": {"pageIndex": 1, "pageSize": 1, "total": 0}, "hotspots": [], "components": []}
NOT_FOUND = b'{"errors":[{"msg":"Analysis with id \\u0027AZpBd5yUcRpLQ0Wq9X1b\\u0027 is not found"}]}'


def task(status: str) -> dict:
    t = {"id": "AZpBd5yUcRpLQ0Wq9X1a", "type": "REPORT", "componentKey": "batleforc_batlehub", "status": status}
    if status == "SUCCESS":
        t["analysisId"] = "AZpBd6Bv3cWcHQZs0x7Q"
    return {"task": t}


def http_error(url: str, code: int, body: bytes) -> urllib.error.HTTPError:
    return urllib.error.HTTPError(url, code, "Not Found", email.message.Message(), io.BytesIO(body))


class Fake:
    """`urlopen` over a table of `path → answer`; an answer may be a list (one per call) or an exception."""

    def __init__(self, routes: dict):
        self.routes = routes
        self.requests: list = []

    def __call__(self, request, timeout=None):
        self.requests.append(request)
        path = urllib.parse.urlsplit(request.full_url).path
        answer = self.routes[path]
        if isinstance(answer, list):
            answer = answer.pop(0) if len(answer) > 1 else answer[0]
        if isinstance(answer, BaseException):
            raise answer
        return io.BytesIO(json.dumps(answer).encode())

    def query(self, path: str) -> dict:
        [req] = [r for r in self.requests if urllib.parse.urlsplit(r.full_url).path == path]
        return dict(urllib.parse.parse_qsl(urllib.parse.urlsplit(req.full_url).query))


def routes(status: dict, issues=NO_ISSUES, hotspots=NO_HOTSPOTS, ce=None) -> dict:
    return {
        "/api/ce/task": ce if ce is not None else task("SUCCESS"),
        "/api/qualitygates/project_status": status,
        "/api/issues/search": issues,
        "/api/hotspots/search": hotspots,
    }


class SonarQualityGate(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.report = self.tmp / "report-task.txt"
        self.report.write_text(REPORT_TASK)
        self.output = self.tmp / "github_output"
        self.output.write_text("")

    def run_main(self, fake: Fake, *argv: str, token: str = "") -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        args = ["sonar_quality_gate.py", "--report-task", str(self.report), *argv]
        env = {"GITHUB_OUTPUT": str(self.output), "SONAR_TOKEN": token}
        with (
            mock.patch.object(sys, "argv", args),
            mock.patch.dict(os.environ, env),
            mock.patch.object(sonar_quality_gate.urllib.request, "urlopen", fake),
            mock.patch.object(sonar_quality_gate.time, "sleep"),
            contextlib.redirect_stdout(out),
            contextlib.redirect_stderr(err),
        ):
            code = sonar_quality_gate.main()
        return code, out.getvalue(), err.getvalue()

    def assertExits(self, fake: Fake, message: str, *argv: str) -> None:
        with self.assertRaises(SystemExit) as cm:
            self.run_main(fake, *argv)
        self.assertIn(message, str(cm.exception.code))
        self.assertEqual(self.output.read_text(), "", "no verdict may be written when there is none")

    def test_error_is_a_successful_run_that_reports_failed(self):
        fake = Fake(routes(ERROR_STATUS, issues={**NO_ISSUES, "total": 45, "paging": {"total": 45}}))
        code, out, _ = self.run_main(fake, "--pull-request", "296")
        self.assertEqual(code, 0)
        self.assertEqual(self.output.read_text(), "status=FAILED\n")
        self.assertTrue(out.startswith("<!-- sonar-quality-gate -->\n"))
        self.assertIn("### SonarCloud quality gate — ❌ **Failed**", out)
        self.assertIn("| ❌ | Reliability rating on new code | D | ≤ A |", out)
        self.assertIn("| ❌ | Security rating on new code | C | ≤ A |", out)
        self.assertNotIn("Coverage on new code", out)  # only failing conditions get a row
        self.assertIn("**45** open issues on new code", out)
        self.assertIn(f"[Full report on SonarCloud]({SERVER}/dashboard?id=batleforc_batlehub&pullRequest=296)", out)
        # The gate is asked about *this* analysis, the counts about this PR.
        self.assertEqual(fake.query("/api/qualitygates/project_status"), {"analysisId": "AZpBd6Bv3cWcHQZs0x7Q"})
        self.assertEqual(fake.query("/api/issues/search")["pullRequest"], "296")
        self.assertEqual(fake.query("/api/hotspots/search")["pullRequest"], "296")

    def test_ok_passes_and_says_how_many_conditions_were_met(self):
        code, out, _ = self.run_main(Fake(routes(OK_STATUS, hotspots={"paging": {"total": 2}})))
        self.assertEqual(code, 0)
        self.assertEqual(self.output.read_text(), "status=PASSED\n")
        self.assertIn("— ✅ **Passed**", out)
        self.assertIn("All 5 conditions met.", out)
        self.assertIn("**0** open issues on new code · **2** security hotspots to review", out)

    def test_warn_is_not_passed(self):
        warn = {"projectStatus": {**OK_STATUS["projectStatus"], "status": "WARN"}}
        code, out, _ = self.run_main(Fake(routes(warn)))
        self.assertEqual(code, 0)
        self.assertEqual(self.output.read_text(), "status=FAILED\n")
        self.assertIn("### SonarCloud quality gate — **WARN**", out)

    def test_no_gate_configured_is_not_passed(self):
        none = {"projectStatus": {"status": "NONE", "conditions": [], "periods": [], "ignoredConditions": False}}
        code, out, _ = self.run_main(Fake(routes(none)))
        self.assertEqual(code, 0)
        # sonar.yaml fails the job on anything but PASSED, so "no gate" goes red.
        self.assertEqual(self.output.read_text(), "status=FAILED\n")
        self.assertIn("— ➖ **No gate configured**", out)
        self.assertNotIn("condition", out)

    def test_http_error_on_the_gate_fails_without_a_verdict(self):
        url = f"{SERVER}/api/qualitygates/project_status"
        err = http_error(url, 404, NOT_FOUND)
        self.addCleanup(err.close)
        fake = Fake(routes(err))
        with self.assertRaises(urllib.error.HTTPError):
            self.run_main(fake)
        self.assertEqual(self.output.read_text(), "")

    def test_http_error_on_the_task_fails_without_a_verdict(self):
        err = http_error(CE_TASK, 401, b'{"errors":[{"msg":"Authentication is required"}]}')
        self.addCleanup(err.close)
        fake = Fake(routes(OK_STATUS, ce=err))
        with self.assertRaises(urllib.error.HTTPError):
            self.run_main(fake)
        self.assertEqual(self.output.read_text(), "")

    def test_counts_that_fail_are_left_out_not_fatal(self):
        down = urllib.error.URLError("connection refused")
        code, out, _ = self.run_main(Fake(routes(OK_STATUS, issues=down, hotspots=ValueError("not json"))))
        self.assertEqual(code, 0)
        self.assertNotIn("open issue", out)
        self.assertNotIn("hotspot", out)
        self.assertEqual(self.output.read_text(), "status=PASSED\n")

    def test_polls_until_the_task_is_processed(self):
        fake = Fake(routes(OK_STATUS, ce=[task("PENDING"), task("IN_PROGRESS"), task("SUCCESS")]))
        code, _, _ = self.run_main(fake)
        self.assertEqual(code, 0)
        self.assertEqual(sum(1 for r in fake.requests if "/api/ce/task" in r.full_url), 3)

    def test_failed_or_canceled_task_exits(self):
        for status in ("FAILED", "CANCELED"):
            with self.subTest(status=status):
                self.assertExits(Fake(routes(OK_STATUS, ce=task(status))), f"the analysis task ended {status}: {CE_TASK}")

    def test_task_still_pending_at_the_deadline_exits(self):
        self.assertExits(Fake(routes(OK_STATUS, ce=task("IN_PROGRESS"))), "the analysis was still IN_PROGRESS after 0s", "--timeout", "0")

    def test_missing_report_task_exits(self):
        self.report.unlink()
        self.assertExits(Fake({}), "report-task.txt not found — the scanner step did not run")

    def test_token_is_the_basic_auth_user(self):
        fake = Fake(routes(OK_STATUS))
        self.run_main(fake, token="squ_abc")
        self.assertEqual(fake.requests[0].get_header("Authorization"), "Basic c3F1X2FiYzo=")  # "squ_abc:"
        fake = Fake(routes(OK_STATUS))
        self.run_main(fake)
        self.assertIsNone(fake.requests[0].get_header("Authorization"))

    def test_branch_analysis_is_not_scoped_and_without_github_output_still_reports(self):
        fake = Fake(routes(OK_STATUS))
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop("GITHUB_OUTPUT", None)
            out, args = io.StringIO(), ["sonar_quality_gate.py", "--report-task", str(self.report)]
            with (
                mock.patch.object(sys, "argv", args),
                mock.patch.object(sonar_quality_gate.urllib.request, "urlopen", fake),
                contextlib.redirect_stdout(out),
            ):
                self.assertEqual(sonar_quality_gate.main(), 0)
        self.assertIn("✅ **Passed**", out.getvalue())
        self.assertNotIn("pullRequest", fake.query("/api/issues/search"))

    def test_render_value(self):
        self.assertEqual(sonar_quality_gate.render_value("new_coverage", "89.4"), "89.4%")
        self.assertEqual(sonar_quality_gate.render_value("new_coverage", "n/a"), "n/a")
        self.assertEqual(sonar_quality_gate.render_value("new_security_rating", "9"), "9")
        self.assertEqual(sonar_quality_gate.render_value("new_violations", "3"), "3")


if __name__ == "__main__":
    unittest.main()
