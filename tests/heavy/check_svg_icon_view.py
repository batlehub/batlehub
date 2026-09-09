#!/usr/bin/env python3
"""What the real Extensions view made of an SVG icon (RFC 0007-bis §11 q1).

Reads the JSONL `vsx_view.mjs --phase icon` prints.

    check_svg_icon_view.py <view.jsonl> <publisher.name> [--print painted|size]

What is **asserted**: the view listed the extension, and the icon it drew is
*this registry's asset* — `…/{publisher}/{name}/{version}/…Icons.Default` — and
not the editor's own `defaultIcon`. That is the half the change is about: an
`application/octet-stream` icon is not advertised as an image, and an entry that
has none falls back to the editor's.

What is **reported and not asserted**: whether the browser painted it.
`naturalWidth` is 0 here for every gallery icon, the fixture's PNG included, and
why is not established. The first answer written down blamed the workbench's
Content-Security-Policy; measuring it disproved that — the page this build
serves carries no CSP, and the refusals the driver counts belong to the readme's
webview iframe. So the driver now reports what became of each icon request and
this prints it, which is the difference between a measurement and a guess.
Requiring a paint before the cause is known would be a red gate for someone
else's reason; the bytes are asserted by `curl` in the step above, where they
can be.

`--print painted` writes `yes`/`no`, `--print size` writes `WxH`; both assert
nothing and exist for the suite's summary line.
"""

import json
import sys


def load(path):
    phases = {}
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            if line.startswith("{"):
                doc = json.loads(line)
                phases[doc["phase"]] = doc
    return phases


def pick(entries, ext_id):
    """The fixture's row, by publisher or by name. `None` if it is not listed.

    No falling back to "the first row": the view answers a new query from its
    own cache first, and a fallback measured the *previous* search's extension
    and called it a pass.
    """
    publisher, name = ext_id.split(".", 1)
    flat = name.lower().replace("-", "").replace(" ", "")
    for entry in entries:
        if entry["publisher"].lower() == publisher.lower():
            return entry
        if entry["name"].lower().replace("-", "").replace(" ", "") == flat:
            return entry
    return None


def main() -> int:
    path, ext_id = sys.argv[1], sys.argv[2]
    rest = sys.argv[3:]
    phases = load(path)

    if "icon" not in phases:
        print("the driver emitted no icon phase", file=sys.stderr)
        return 1
    entries = phases["icon"]["entries"]
    # What became of the icon requests, printed either way. `naturalWidth: 0`
    # cannot tell "the request never left" from "it came back a 404" from "it
    # arrived and would not decode", and the first cause written down for this
    # was a guess that measuring disproved.
    traffic = phases["icon"].get("iconTraffic") or []
    if "--print" not in rest:
        print(f"icon requests observed: {traffic if traffic else 'none — the browser asked for no icon at all'}")
    row = pick(entries, ext_id) if entries else None
    icon = (row or {}).get("icon") or {}

    if "--print" in rest:
        what = rest[rest.index("--print") + 1]
        if what == "size":
            print(f"{icon.get('naturalWidth', 0)}x{icon.get('naturalHeight', 0)}")
        else:
            print("yes" if icon.get("naturalWidth", 0) > 0 else "no")
        return 0

    if row is None:
        listed = [e["name"] for e in entries]
        print(f"the view did not list {ext_id}; it listed {listed}", file=sys.stderr)
        return 1
    if not icon.get("src"):
        print(f"the entry has no icon element at all: {row}", file=sys.stderr)
        return 1

    publisher, name = ext_id.split(".", 1)
    src = icon["src"]
    # *Our* asset, not the editor's fallback: VS Code swaps in its own
    # `defaultIcon.png` for an entry whose gallery advertises no icon, and that
    # one decodes perfectly — so a check on the paint alone would pass most
    # convincingly in exactly the case this step exists to catch.
    if "Icons.Default" not in src or f"/{publisher}/{name}/" not in src:
        print(
            f"the entry is not showing this registry's icon for {ext_id}: {src}",
            file=sys.stderr,
        )
        return 1

    painted = icon.get("naturalWidth", 0) > 0
    print(
        f"{ext_id}: the view adopted the registry's icon ({src}); "
        f"painted: {'yes' if painted else 'no — see the icon requests above for why'}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
