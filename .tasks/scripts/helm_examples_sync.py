#!/usr/bin/env python3
"""The inline recipes in the Helm page *are* the files under `deploy/helm/`.

Two places carrying the same YAML is the arrangement this repository keeps
finding drifted — the two documentation trees RFC 0005 merged, the token copy
nothing read. Here the file is canonical: it is what `-f` installs and what
`task helm:examples` renders, so the page is written from it rather than beside
it.

    --check   fail if a page's block differs from its file (the gate)
    (default) write each file into its page's block

Only the English page: the French one translates the YAML comments, and its
freshness is what `task docs:i18n:check` is for. Setup 2 is deliberately
partial — the blocks that differ from Setup 1 — so it has no pair here.
"""
import pathlib, re, sys

PAIRS = ["minimal", "production"]
PAGE = pathlib.Path("docs/guide/install/helm.md")


def body(text: str) -> str:
    """A values file without its leading comment header."""
    lines = text.split("\n")
    i = 0
    while i < len(lines) and (lines[i].startswith("#") or not lines[i].strip()):
        i += 1
    return "\n".join(lines[i:]).strip("\n")


def main(check: bool) -> int:
    page = PAGE.read_text()
    drifted = []
    for name in PAIRS:
        f = pathlib.Path("deploy/helm/values-%s.yaml" % name)
        want = body(f.read_text())
        pattern = re.compile(
            r"(```yaml\n# deploy/helm/values-%s\.yaml\n)(.*?)(```)" % name, re.S
        )
        m = pattern.search(page)
        if not m:
            print("%s: no inline block named %s" % (PAGE, f), file=sys.stderr)
            return 1
        if m.group(2).strip("\n") == want:
            continue
        drifted.append(str(f))
        page = pattern.sub(lambda m: m.group(1) + want + "\n" + m.group(3), page, count=1)

    if not drifted:
        print("every inline recipe matches the file it names")
        return 0
    if check:
        for f in drifted:
            print("%s has drifted from %s" % (PAGE, f), file=sys.stderr)
        print("run `task helm:examples:sync` and commit the result", file=sys.stderr)
        return 1
    PAGE.write_text(page)
    print("wrote %s into %s" % (", ".join(drifted), PAGE))
    return 0


sys.exit(main("--check" in sys.argv))
