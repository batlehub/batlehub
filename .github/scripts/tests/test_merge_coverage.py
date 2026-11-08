"""merge_coverage.py's table and its floors (RFC 0041 §6.1, decisions 39 and 40).

The LCOV is written as cargo-llvm-cov writes it: absolute `SF:` paths, one
`FN`/`FNDA` pair per function, `DA:<line>,<count>`.
"""

import contextlib
import io
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import merge_coverage  # noqa: E402

ROOT = "/home/runner/work/batlehub/batlehub"


def lcov(files: dict[str, tuple[int, int]]) -> str:
    """`{rel_path: (hit, total)}` → an LCOV whose first `hit` lines ran."""
    out = []
    for rel, (hit, total) in files.items():
        out.append(f"SF:{ROOT}/{rel}")
        out.append("FN:1,f")
        out.append(f"FNDA:{1 if hit else 0},f")
        out += [f"DA:{n},{1 if n <= hit else 0}" for n in range(1, total + 1)]
        out.append("end_of_record")
    return "\n".join(out) + "\n"


class MergeCoverage(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())

    def write(self, name: str, text: str) -> Path:
        path = self.tmp / name
        path.write_text(text)
        return path

    def run_main(self, *argv: str) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        args = ["merge_coverage.py", "--output", str(self.tmp / "merged.lcov"), *argv]
        with mock.patch.object(sys, "argv", args), contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = merge_coverage.main()
        return code, out.getvalue(), err.getvalue()

    def test_no_floor_reports_and_passes_with_an_absent_category(self):
        unit = self.write("unit.lcov", lcov({"crates/core/src/a.rs": (9, 10)}))
        code, out, _ = self.run_main(f"unit={unit}", f"heavy={self.tmp / 'missing.lcov'}")
        self.assertEqual(code, 0)
        self.assertIn("| heavy | — | absent | — | absent |", out)
        self.assertIn("| `crates/core` | 90.00% | 9/10 |", out)

    def test_floor_met_counts_unit_and_integration_only(self):
        unit = self.write("unit.lcov", lcov({"crates/core/src/rules/a.rs": (9, 10)}))
        integ = self.write("integ.lcov", lcov({"crates/core/src/rules/a.rs": (9, 10)}))
        # heavy covers the tenth line; it must not lift the floor computation.
        heavy = self.write("heavy.lcov", lcov({"crates/core/src/rules/a.rs": (10, 10)}))
        code, out, err = self.run_main("--floor", "crates/core/src/rules=90", f"unit={unit}", f"integration={integ}", f"heavy={heavy}")
        self.assertEqual((code, err.count("coverage floor:")), (0, 0), err)
        self.assertIn("Coverage floors: 1 met", out)
        code, _, err = self.run_main("--floor", "crates/core/src/rules=91", f"unit={unit}", f"integration={integ}", f"heavy={heavy}")
        self.assertEqual(code, 1)
        self.assertIn("`crates/core/src/rules` at 90.00% lines (9/10), floor 91%", err)

    def test_breach_names_the_three_least_covered_files_and_the_reproduction(self):
        files = {f"crates/web/src/middleware/{n}.rs": (h, 10) for n, h in [("a", 10), ("b", 2), ("c", 5), ("d", 7)]}
        files["crates/web/src/middlewarex.rs"] = (0, 50)  # shares the prefix's spelling, not its directory
        unit = self.write("unit.lcov", lcov(files))
        code, out, err = self.run_main("--floor", "crates/web/src/middleware/=90", "--floor-from", "unit", f"unit={unit}")
        self.assertEqual(code, 1)
        [line] = [l for l in err.splitlines() if l.startswith("coverage floor:")]
        self.assertIn("at 60.00% lines (24/40)", line)
        self.assertIn("middleware/b.rs 20.00%, crates/web/src/middleware/c.rs 50.00%, crates/web/src/middleware/d.rs 70.00%", line)
        self.assertNotIn("middlewarex", line)
        self.assertIn("task coverage-check", line)
        self.assertIn('--ignore-filename-regex "$COVERAGE_EXCLUDE"', line)
        self.assertIn("### Coverage floors", out)

    def test_one_line_per_breach_and_total(self):
        unit = self.write("unit.lcov", lcov({"crates/core/src/a.rs": (5, 10), "crates/web/src/b.rs": (7, 10)}))
        code, _, err = self.run_main("--floor", "total=80", "--floor", "crates/core/src/a.rs=90", "--floor", "crates/web=70", "--floor-from", "unit", f"unit={unit}")
        self.assertEqual(code, 1)
        lines = [l for l in err.splitlines() if l.startswith("coverage floor:")]
        self.assertEqual(len(lines), 2, lines)
        self.assertIn("`total` at 60.00%", lines[0])
        self.assertIn("`crates/core/src/a.rs` at 50.00%", lines[1])

    def test_floors_file_with_comments(self):
        floors = self.write("floors.txt", "# tier 1\ncrates/core/src/a.rs=90  # integrity\n\ntotal=80\n")
        unit = self.write("unit.lcov", lcov({"crates/core/src/a.rs": (95, 100)}))
        code, out, _ = self.run_main("--floors-file", str(floors), "--floor-from", "local", f"local={unit}")
        self.assertEqual(code, 0)
        self.assertIn("Coverage floors: 2 met (on local)", out)

    def test_prefix_absent_from_the_lcov_is_skipped_not_zero(self):
        unit = self.write("unit.lcov", lcov({"crates/core/src/a.rs": (9, 10)}))
        code, _, err = self.run_main("--floor", "crates/web/src/extractors.rs=90", "--floor-from", "local", f"local={unit}")
        self.assertEqual(code, 0)
        self.assertIn("`crates/web/src/extractors.rs` is not in the LCOV, skipped", err)

    def test_absent_floor_category_fails_closed(self):
        unit = self.write("unit.lcov", lcov({"crates/core/src/a.rs": (10, 10)}))
        code, _, err = self.run_main("--floor", "total=80", f"unit={unit}", f"integration={self.tmp / 'missing.lcov'}")
        self.assertEqual(code, 1)
        self.assertIn("category `integration` has no LCOV — see job `integration`", err)
        self.assertNotIn("at ", err.split("see job")[1])  # the floors are not computed on half the data

    def test_floor_category_never_named_fails_closed(self):
        unit = self.write("unit.lcov", lcov({"crates/core/src/a.rs": (10, 10)}))
        code, _, err = self.run_main("--floor", "total=80", f"unit={unit}")
        self.assertEqual(code, 1)
        self.assertIn("category `integration` has no LCOV", err)

    def test_malformed_floor_and_missing_floors_file_fail(self):
        unit = self.write("unit.lcov", lcov({"crates/core/src/a.rs": (10, 10)}))
        for argv in (["--floor", "total"], ["--floor", "total=180"], ["--floor", "=80"], ["--floors-file", str(self.tmp / "nope.txt")]):
            with self.subTest(argv=argv):
                code, _, err = self.run_main(*argv, f"unit={unit}")
                self.assertEqual(code, 1)
                self.assertIn("coverage floor:", err)

    def test_rel_path(self):
        self.assertEqual(merge_coverage.rel_path(f"{ROOT}/crates/core/src/a.rs"), "crates/core/src/a.rs")
        self.assertEqual(merge_coverage.rel_path("server/src/main.rs"), "server/src/main.rs")
        self.assertEqual(merge_coverage.rel_path("/x/y.rs"), "/x/y.rs")


if __name__ == "__main__":
    unittest.main()
