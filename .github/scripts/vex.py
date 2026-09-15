#!/usr/bin/env python3
"""The VEX document: check it, and bind it to a release.

A CVE reported against a shipped image is a question — *does it reach anything
this image actually runs?* — and the answer lives in someone's head until it is
written down. `.trivyignore.yaml` already records the answers this project has
given, but in a form only Trivy reads: a consumer of the image, an auditor, or
any other scanner sees the finding and nothing else.

This is the other half. `vex/batlehub.openvex.json` states the same answers in
OpenVEX, which every mainstream scanner reads, and the release binds a rendered
copy of it to the images it published:

    vex.py check                       # the gate: structure, and the policy below
    vex.py render --version 1.2.3 \\    # the release artifact: products become
        --image ghcr.io/o/r@sha256:… \\  # the digests actually published
        --out batlehub-1.2.3.openvex.json

**The policy the check enforces**, beyond the schema:

* every statement carries a `timestamp`, so "when was this decided" is never a
  guess;
* `not_affected` carries one of OpenVEX's five justifications *and* an
  `impact_statement` in prose — the justification is for the scanner, the prose
  is for the person who has to believe it;
* `affected` carries an `action_statement`, because a status with no remedy is
  a note rather than a statement;
* every `.trivyignore.yaml` entry has a statement here covering the same CVE,
  and vice versa. Two suppression mechanisms that can disagree eventually will,
  and the one nobody reads is the one that goes stale.

Not enforced, deliberately: that a statement ever expires. `.trivyignore.yaml`
carries the expiry — it is the gate — and duplicating the date here would give
it two homes and one of them would be wrong.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from datetime import datetime, timezone
from pathlib import Path

# https://github.com/openvex/spec — the five justifications a not_affected
# statement may carry. Anything else is a typo or an invention, and a scanner
# that does not recognise it silently ignores the statement.
JUSTIFICATIONS = {
    "component_not_present",
    "vulnerable_code_not_present",
    "vulnerable_code_not_in_execute_path",
    "vulnerable_code_cannot_be_controlled_by_adversary",
    "inline_mitigations_already_exist",
}
STATUSES = {"not_affected", "affected", "fixed", "under_investigation"}

DEFAULT_DOC = Path("vex/batlehub.openvex.json")
DEFAULT_TRIVYIGNORE = Path(".trivyignore.yaml")


def fail(problems: list[str]) -> int:
    for p in problems:
        print(f"✗ {p}", file=sys.stderr)
    print(f"\n{len(problems)} problem(s) in the VEX document.", file=sys.stderr)
    return 1


def load(path: Path) -> dict:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        raise SystemExit(f"{path} does not exist")
    except json.JSONDecodeError as e:
        raise SystemExit(f"{path} is not valid JSON: {e}")


def trivyignore_ids(path: Path) -> set[str]:
    """The CVE ids `.trivyignore.yaml` suppresses.

    Read with a regex rather than a YAML parser on purpose: this script runs in
    CI where PyYAML is not guaranteed, the file's shape is two levels deep, and
    an id is the only thing being extracted. A malformed file here shows up as
    a missing id and a loud mismatch, not as a silent pass.
    """
    if not path.exists():
        return set()
    return set(re.findall(r"^\s*-\s*id:\s*([A-Za-z0-9\-_]+)\s*$", path.read_text(), re.M))


def check(doc_path: Path, trivyignore: Path) -> int:
    doc = load(doc_path)
    problems: list[str] = []

    if not str(doc.get("@context", "")).startswith("https://openvex.dev/ns/"):
        problems.append("@context is not an OpenVEX namespace")
    for field in ("@id", "author", "timestamp", "version"):
        if not doc.get(field):
            problems.append(f"the document has no {field}")

    statements = doc.get("statements")
    if not isinstance(statements, list) or not statements:
        return fail(problems + ["the document carries no statements"])

    seen: set[str] = set()
    for i, st in enumerate(statements):
        where = f"statement {i}"
        vuln = (st.get("vulnerability") or {}).get("name")
        if not vuln:
            problems.append(f"{where}: no vulnerability name")
        else:
            where = f"{vuln}"
            seen.add(vuln)

        status = st.get("status")
        if status not in STATUSES:
            problems.append(f"{where}: status {status!r} is not one of {sorted(STATUSES)}")

        if not st.get("timestamp"):
            problems.append(f"{where}: no timestamp — when was this decided?")

        products = st.get("products") or []
        if not products:
            problems.append(f"{where}: names no product, so it applies to nothing")
        for product in products:
            if not product.get("@id"):
                problems.append(f"{where}: a product has no @id")

        if status == "not_affected":
            justification = st.get("justification")
            if justification not in JUSTIFICATIONS:
                problems.append(
                    f"{where}: justification {justification!r} is not one of "
                    f"{sorted(JUSTIFICATIONS)} — a scanner ignores a statement it "
                    "cannot read"
                )
            if not st.get("impact_statement"):
                problems.append(
                    f"{where}: not_affected with no impact_statement — the justification "
                    "is for the scanner, the prose is for the person who has to believe it"
                )
        if status == "affected" and not st.get("action_statement"):
            problems.append(f"{where}: affected with no action_statement")

    ignored = trivyignore_ids(trivyignore)
    for cve in sorted(ignored - seen):
        problems.append(
            f"{cve} is suppressed in {trivyignore} but has no VEX statement — "
            "the reason exists, it is just not in the form a consumer can read"
        )
    for cve in sorted(seen - ignored):
        st = next(s for s in statements if (s.get("vulnerability") or {}).get("name") == cve)
        # `fixed` and `under_investigation` say nothing about the gate, so they
        # are not expected to have an ignore entry. A standing `not_affected`
        # that the gate still fails on is the mismatch worth reporting.
        if st.get("status") == "not_affected":
            problems.append(
                f"{cve} is stated not_affected but {trivyignore} does not ignore it — "
                "either the gate is still failing on it, or the statement outlived its reason"
            )

    if problems:
        return fail(problems)
    print(
        f"✓ {doc_path}: {len(statements)} statement(s), "
        f"{len(seen & ignored)} matched against {trivyignore}"
    )
    return 0


def render(doc_path: Path, version: str, images: list[str], out: Path) -> int:
    """Rewrite the products as the digests this release actually published.

    The source document names products by repository (`pkg:oci/batlehub-worker`)
    because that is what a human maintains. A consumer needs the opposite: the
    exact image they pulled. So the release renders one document per release,
    replacing each product with the matching `--image`'s purl — and anything
    that does not match is dropped rather than shipped pointing at nothing.
    """
    doc = load(doc_path)
    by_repo: dict[str, str] = {}
    for image in images:
        ref, _, digest = image.partition("@")
        if not digest:
            raise SystemExit(f"--image {image} must be <repository>@sha256:… (a digest, not a tag)")
        repo = ref.rsplit("/", 1)[-1]
        # `pkg:oci/<name>@<digest>?repository_url=<registry/path>` — the purl
        # spelling Trivy, Grype and syft all emit for a container image.
        by_repo[repo] = f"pkg:oci/{repo}@{digest}?repository_url={ref}"

    now = datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")
    doc["@id"] = f"{doc['@id']}-{version}"
    doc["timestamp"] = now
    doc["version"] = int(doc.get("version", 1))

    kept = []
    for st in doc.get("statements", []):
        products = []
        for product in st.get("products", []):
            repo = str(product.get("@id", "")).removeprefix("pkg:oci/")
            if repo in by_repo:
                product = dict(product, **{"@id": by_repo[repo]})
                products.append(product)
            else:
                print(
                    f"  dropping product {product.get('@id')} from "
                    f"{(st.get('vulnerability') or {}).get('name')}: this release published no "
                    "such image",
                    file=sys.stderr,
                )
        if products:
            st = dict(st, products=products)
            kept.append(st)
    doc["statements"] = kept

    if not kept:
        # Not an error: a release whose images carry none of the stated
        # vulnerabilities is the outcome this whole file exists to reach. The
        # document is still written, and still attested, so the absence is
        # something a consumer can verify rather than infer.
        print("  no statement applies to the images given — writing an empty document")

    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(doc, indent=2) + "\n", encoding="utf-8")
    print(f"✓ {out}: {len(kept)} statement(s) for {version}")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="command", required=True)

    c = sub.add_parser("check", help="validate the document and its agreement with .trivyignore.yaml")
    c.add_argument("--doc", type=Path, default=DEFAULT_DOC)
    c.add_argument("--trivyignore", type=Path, default=DEFAULT_TRIVYIGNORE)

    r = sub.add_parser("render", help="bind the document to the images a release published")
    r.add_argument("--doc", type=Path, default=DEFAULT_DOC)
    r.add_argument("--version", required=True)
    r.add_argument(
        "--image",
        action="append",
        default=[],
        required=True,
        help="a published image as <repository>@<digest>; repeatable",
    )
    r.add_argument("--out", type=Path, required=True)

    args = ap.parse_args()
    if args.command == "check":
        return check(args.doc, args.trivyignore)
    return render(args.doc, args.version, args.image, args.out)


if __name__ == "__main__":
    raise SystemExit(main())
