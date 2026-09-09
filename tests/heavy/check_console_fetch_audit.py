#!/usr/bin/env python3
"""The fetch is in the audit, and it names the reader who pressed the button.

    check_console_fetch_audit.py <audit-log.json> <name> <version> <user_id>

This is the assertion that fails against a "reuse the warming service"
implementation, which is why RFC 0007-bis §5.3 exists: warming records no event,
and the one it would record would name an administrator. Read back through
`GET /api/v1/admin/audit-log` rather than out of the database, so the suite is
held to what an operator can actually see.
"""

import json
import sys


def main() -> int:
    path, name, version, want_user = sys.argv[1:5]
    with open(path, encoding="utf-8") as fh:
        doc = json.load(fh)
    events = doc["items"]

    rows = [
        e
        for e in events
        if (e.get("package_id") or {}).get("name") == name
        and (e.get("package_id") or {}).get("version") == version
    ]
    if not rows:
        seen = [
            ((e.get("package_id") or {}).get("name"), (e.get("package_id") or {}).get("version"))
            for e in events[:5]
        ]
        print(f"no audit row for {name} {version}; saw {seen}", file=sys.stderr)
        return 1

    actors = {e.get("user_id") for e in rows}
    if want_user not in actors:
        print(
            f"the fetch was not attributed to {want_user}, the reader who "
            f"pressed it: {sorted(a for a in actors if a)}",
            file=sys.stderr,
        )
        return 1
    actions = sorted({str(e.get("action")) for e in rows})
    print(f"{len(rows)} audit row(s) for {name} {version}, actions {actions}, actor {want_user}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
