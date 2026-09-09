#!/usr/bin/env python3
"""Assert what the console's catalogue drew and did (RFC 0007-bis §11 q3).

Reads the JSONL `console_fetch.mjs` prints.

    check_console_fetch.py <run.jsonl>              signed-in run: the button
                                                    named a version, the click
                                                    landed, the row changed
    check_console_fetch.py <run.jsonl> --anonymous  no row offers a fetch
    check_console_fetch.py <run.jsonl> --print KEY  name | version | label

`--print name` decodes the package out of the row's own link
(`/packages/{registry}/{name}`) rather than out of the button's label: the label
is translated, and a suite that parsed it would be asserting the French
build's wording on a French browser.
"""

import json
import sys
from urllib.parse import unquote


def load(path):
    phases = {}
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            if line.startswith("{"):
                doc = json.loads(line)
                phases[doc["phase"]] = doc
    return phases


def coordinate(click):
    """`(name, version)` for the row that was clicked."""
    href = click.get("href") or ""
    # `/packages/{registry}/{name}` — the name is one segment, percent-encoded,
    # so a scoped or slashed name survives the round trip.
    parts = href.split("/packages/", 1)
    name = unquote(parts[1].split("/", 1)[1]) if len(parts) == 2 and "/" in parts[1] else ""
    return name, click.get("version", "")


def check_anonymous(phases):
    listing = phases.get("listing")
    if listing is None:
        print("the driver emitted no listing phase", file=sys.stderr)
        return 1
    rows = listing["rows"]
    if not rows:
        print(
            "the anonymous catalogue listed nothing at all — this asserts "
            "the absence of a button, so an empty table would pass for the "
            "wrong reason",
            file=sys.stderr,
        )
        return 1
    offered = [r for r in rows if r.get("button")]
    if offered:
        print(f"an anonymous reader was offered a fetch: {offered[0]}", file=sys.stderr)
        return 1
    print(f"{len(rows)} row(s), none offering a fetch")
    return 0


def check_signed_in(phases):
    listing = phases.get("listing")
    click = phases.get("click")
    if listing is None or click is None:
        print("the driver emitted no listing/click phase", file=sys.stderr)
        return 1
    if not click.get("clicked"):
        print(f"the button was never pressed: {click.get('reason', click)}", file=sys.stderr)
        return 1

    label = click.get("label", "")
    name, version = coordinate(click)
    if not name or not version:
        print(f"the clicked row named no coordinate: {click}", file=sys.stderr)
        return 1
    # The point of the whole question: the label carries the version, so the
    # reader knows what they are asking for and the page chooses nothing.
    if version not in label and version not in click.get("ariaLabel", ""):
        print(
            f"the button did not name the version it would fetch: "
            f"label={label!r} aria={click.get('ariaLabel')!r} version={version!r}",
            file=sys.stderr,
        )
        return 1
    # This package's row leaves the upstream half: it is gone from the table,
    # or it is back with a held state and no button. Found by its own link, not
    # by scanning the table for any change — another row changing is not this
    # button working. Compared on the whole row text because the state chip, the
    # size cell and the button all move, and any one alone could honestly stay.
    mine = next((r for r in click.get("rows", []) if r.get("href") == click.get("href")), None)
    if mine is not None and mine["text"] == click["before"]:
        print(f"the row is unchanged after the fetch: {click['before']!r}", file=sys.stderr)
        return 1
    landed = "left the table" if mine is None else f"now {mine['state']!r}"
    print(f"pressed {label!r} → {name} {version}; the row {landed}")
    return 0


def main() -> int:
    path = sys.argv[1]
    rest = sys.argv[2:]
    phases = load(path)

    if "--print" in rest:
        key = rest[rest.index("--print") + 1]
        click = phases.get("click", {})
        name, version = coordinate(click)
        print({"name": name, "version": version, "label": click.get("label", "")}[key])
        return 0
    if "--anonymous" in rest:
        return check_anonymous(phases)
    return check_signed_in(phases)


if __name__ == "__main__":
    sys.exit(main())
