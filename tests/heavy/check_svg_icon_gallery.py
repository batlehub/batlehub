#!/usr/bin/env python3
"""The gallery lists the icon fixture and advertises its icon asset.

    check_svg_icon_gallery.py <extensionquery-response.json> <publisher.name>

Asserted before the browser is asked anything, so a failure here names the
server rather than the editor: an entry the gallery does not advertise an
`Icons.Default` asset for is one VS Code draws its own `defaultIcon` for, and no
amount of clicking in the view will change that.
"""

import json
import sys

ICON_ASSET = "Microsoft.VisualStudio.Services.Icons.Default"


def main() -> int:
    path, ext_id = sys.argv[1:3]
    publisher, name = ext_id.split(".", 1)
    doc = json.load(open(path, encoding="utf-8"))

    extensions = [e for r in doc.get("results", []) for e in r.get("extensions", [])]
    if not extensions:
        print(f"the gallery listed nothing for {ext_id}", file=sys.stderr)
        return 1

    mine = next(
        (
            e
            for e in extensions
            if e.get("extensionName") == name
            and (e.get("publisher") or {}).get("publisherName") == publisher
        ),
        None,
    )
    if mine is None:
        listed = [e.get("extensionName") for e in extensions]
        print(f"the gallery did not list {ext_id}; it listed {listed}", file=sys.stderr)
        return 1

    files = [f for v in mine.get("versions", []) for f in v.get("files", [])]
    icon = next((f for f in files if f.get("assetType") == ICON_ASSET), None)
    if icon is None:
        print(
            f"{ext_id} is listed with no {ICON_ASSET} asset; the editor will "
            f"draw its own default. Assets: {sorted({f.get('assetType') for f in files})}",
            file=sys.stderr,
        )
        return 1
    print(f"{ext_id}: the gallery advertises {ICON_ASSET} at {icon.get('source')}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
