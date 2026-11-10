# TD-0005 — 22 registry clients build upstream URLs without encoding the coordinate

| | |
| --- | --- |
| **Status** | Open |
| **Found** | 2026-11-10, `server/src/name_injection_probe.rs` (PR #306), after the forge instance of the same defect (inkdex VULN-SSRF, PR #304) |
| **Area** | `crates/adapters/src/registry/*` (every client but the forges, Galaxy, devfile, Helm and the JetBrains IDE repository) |

## What was observed

actix decodes `%3F` and `%23` in a path parameter, and `validate_path_safe`
lets `?` and `#` through. A client that interpolates a name, a version or a
file name into its upstream URL without encoding it sends `?` as the start of
a query and drops everything from `#` on. The upstream is then asked about
another coordinate than the one authorization and the rules judged.

The probe aims every kind's real client at a recorder with coordinates holding
`?inj=1` and `#frag`. Before PR #306, 22 of 29 kinds misrouted them, 333
requests in all: npm, PyPI, cargo, Go, Maven, NuGet, RubyGems, conda,
Composer, Terraform, Open VSX, VS Code Marketplace, nix, rustup, SDKMAN,
Node.js, deb, rpm, pacman, apk, JetBrains and generic. Measured impact, with
the fix removed: `/proxy/npm-b/pkg/1.0.0%3F/tarball` answered `200` while
`pkg@1.0.0` was blocked.

## What holds it today

PR #306 **refuses** the two characters instead of encoding them:
`reject_url_delimiters(registry_type, …)` in `ProxyService::request_prelude`,
and again in `CoordinateGuard`, which wraps every client
`build_registry_client` returns. The rule is per kind, because some kinds put
one there on purpose: a forge's tag may hold `#`; SDKMAN, devfile and the
JetBrains Marketplace name a document with the query their handler composes
or relays; Galaxy addresses a role as `role#<id>`.

No ecosystem's names, versions or file names use either character, so the
refusal costs no legitimate request. It is still one check standing between a
caller and 22 unencoded URL builders, and the clients stay wrong on their own
terms: a path that reaches one without the guard (a new caller, a test double
swapped for a real client) is the defect again.

## What would retire it

1. In each of the 22 clients, percent-encode every caller-derived segment
   where the URL is built — `percent_encode_path` keeps `/`, as the GitHub and
   Forgejo clients now do (PR #304) and GitLab always did. Each kind has its
   own spelling to keep: Go's module-path escaping, PEP 503 names, Maven's
   group-as-path, npm's scoped `@scope%2Fname`.
2. Do not encode in the guard instead: a client that already encodes would
   turn `%3F` into `%253F`, and the three kinds that compose a query need the
   real `?`.
3. Give the probe a mode that builds the clients **without** `CoordinateGuard`
   and require zero misrouted requests there too, one kind at a time: the
   kinds already encoding pass from the start, and each fixed client is added
   to the list the mode asserts. When all 22 are on it, the guard becomes
   defence in depth rather than the only defence, and this entry is Paid.
