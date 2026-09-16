#!/usr/bin/env python3
"""Reduce a CycloneDX SBOM to scannable coordinates, in scanner-sized pieces.

Two problems sit between `syft` and `vuln.mlab.sh`, and this solves both.

**Size.** syft emits a faithful SBOM: every component carries a `bom-ref`, a
generated `cpe`, a `syft:location` per file it was found in and a `syft:cpe23`
property per name variant it guessed. That is the right output for an artifact
you attest and keep, and far too much to post to a scan endpoint — `ui/` comes
to 896 KB, which the endpoint refuses with HTTP 413. Matching a component
against an advisory needs its coordinates and nothing else, so `type`, `name`,
`version` and `purl` are kept and the rest dropped: about 12x smaller, with
nothing lost that the scanner consults.

**The 512-package cap.** The endpoint reads at most 512 components per request,
warns once, and then reports the remainder as though it were the whole tree —
so a truncated scan comes back looking clean. `ui/` holds 691 packages, which
would leave 179 of them unscanned and unmentioned in the result. Splitting the
document into pieces of at most 512 and scanning each is what makes the answer
cover the tree it claims to.

Reads a CycloneDX JSON document on stdin, writes one file per piece, and prints
each path it wrote on stdout — one per line, ready to hand to the scanner's
`path` input.
"""

import argparse
import json
import sys

# The endpoint's own limit. A piece of exactly this size is accepted whole; the
# scanner only warns once a *single* document exceeds it.
MAX_COMPONENTS = 512


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--out-prefix",
        required=True,
        help="Path prefix for the pieces; each is written as <prefix>.<n>.cdx.json.",
    )
    parser.add_argument(
        "--chunk",
        type=int,
        default=MAX_COMPONENTS,
        help=f"Components per piece (default {MAX_COMPONENTS}).",
    )
    args = parser.parse_args()

    doc = json.load(sys.stdin)

    slim = []
    for component in doc.get("components") or []:
        # A component with no purl cannot be matched against an advisory, and
        # sending it only spends payload budget. syft gives every npm package
        # one; anything without is a file-level artifact, not a dependency.
        purl = component.get("purl")
        if not purl:
            continue
        slim.append(
            {
                "type": component.get("type", "library"),
                "name": component.get("name"),
                "version": component.get("version"),
                "purl": purl,
            }
        )

    # An empty tree still gets one (empty) piece rather than no files at all: a
    # scan step handed no paths silently scans nothing, which is the failure
    # this whole script exists to make impossible.
    pieces = [slim[i : i + args.chunk] for i in range(0, len(slim), args.chunk)] or [[]]

    for number, components in enumerate(pieces, start=1):
        path = f"{args.out_prefix}.{number}.cdx.json"
        with open(path, "w", encoding="utf-8") as handle:
            json.dump(
                {
                    "bomFormat": "CycloneDX",
                    # 1.6 rather than the 1.7 syft emits: 1.7 is recent enough
                    # that a consumer may not know it, and nothing kept here is
                    # newer than 1.6. Declaring the older schema is the
                    # conservative direction.
                    "specVersion": "1.6",
                    "version": 1,
                    "components": components,
                },
                handle,
                separators=(",", ":"),
            )
        print(path)

    print(
        f"{args.out_prefix}: {len(slim)} components in {len(pieces)} piece(s)",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
