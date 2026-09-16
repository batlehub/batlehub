#!/usr/bin/env python3
"""The list of things this pull request's diff implies, as a comment on it.

Not a style checker and not a gate: every other workflow in this directory
already fails on what it can decide mechanically. This answers the question
those cannot, which is *"what does a reviewer have to look for in **this**
diff"* — and it answers it from the paths that changed, because in this
repository the obligations really are path-shaped. A new variant in
`registry_kind.rs` owes nine things in eight files, a file under
`migrations/` owes an entry in `migrations.rs`, and a `values.yaml` edit owes a
regenerated chart README. Those rules are written down (CLAUDE.md,
`docs/contributing/adding-a-registry.md`) and are exactly the ones a hurried
author skips.

Two kinds of line come out of it:

  - **`- [ ] item`** — do this, or confirm it does not apply.
  - **`- [ ] ⚠ item`** — the diff says you probably have not: you changed the
    thing that obliges it and not the file that discharges it. A generated
    file that did not move with its source, a migration with no `mig!` entry,
    a new registry kind with no entry in the UI's type table.

The second kind is the reason this exists. It is still advisory — a human
decides, and plenty of diffs have a good reason — so nothing here fails a
build. It is a checklist, not a gate.

Usage:
  git diff --name-only origin/main...HEAD | python3 .github/scripts/pr_checklist.py
  python3 .github/scripts/pr_checklist.py --files a/b.rs c/d.ts     # for testing
"""

from __future__ import annotations

import argparse
import fnmatch
import sys
from dataclasses import dataclass, field

MARKER = "<!-- pr-checklist -->"


def matches(paths: list[str], *globs: str) -> list[str]:
    """Every changed path matching any of `globs`."""
    hit: list[str] = []
    for path in paths:
        if any(fnmatch.fnmatch(path, g) for g in globs):
            hit.append(path)
    return hit


@dataclass
class Item:
    """One line. `needs` makes it a warning when none of those paths changed."""

    text: str
    needs: tuple[str, ...] = ()

    def render(self, paths: list[str]) -> str:
        if self.needs and not matches(paths, *self.needs):
            return f"- [ ] ⚠ {self.text}"
        return f"- [ ] {self.text}"


@dataclass
class Rule:
    """A section, shown when `when` matches something in the diff."""

    title: str
    when: tuple[str, ...]
    items: list[Item]
    why: str = ""
    # Paths that must *not* be the only match — used to keep a broad rule from
    # firing on a diff that only touched the narrow thing it is about.
    unless_only: tuple[str, ...] = field(default_factory=tuple)

    def fires(self, paths: list[str]) -> bool:
        hit = matches(paths, *self.when)
        if not hit:
            return False
        if self.unless_only and set(hit) <= set(matches(paths, *self.unless_only)):
            return False
        return True


# ── The rules ────────────────────────────────────────────────────────────────
#
# Each one is a rule this repository already states somewhere — CLAUDE.md, the
# contributing guide, an RFC — restated as the question a reviewer asks. Adding
# one is a four-line dataclass; the test for it is `--files`.

RULES: list[Rule] = [
    Rule(
        title="A registry kind changed",
        why="`RegistryKind` is the spine: nine places follow from a new variant, "
        "and the compiler only forces two of them.",
        when=("crates/core/src/entities/registry_kind.rs",),
        items=[
            Item(
                "`ui/src/config/registryTypes.ts` has a `RegistryTypeDef` with setup snippets",
                needs=("ui/src/config/registryTypes.ts",),
            ),
            Item(
                "The `registry-<name>` feature is declared in `crates/adapters/Cargo.toml` "
                "**and** added to `default` — without the declaration the `cfg` never activates",
                needs=("crates/adapters/Cargo.toml",),
            ),
            Item(
                "The client is constructed in `server/src/builders.rs` and wired in `server/src/main.rs`",
                needs=("server/src/builders.rs", "server/src/main.rs"),
            ),
            Item(
                "Driven **live** by a real client: a `phase_<name>` in `tests/heavy/closed_world.sh`, "
                "its `[[registries]]` block, and a `- phase:` row in `test.yaml` — asserting on the "
                "wire transcript, not just the client's exit code",
                needs=("tests/heavy/closed_world.sh", "tests/heavy/config.closed-world.toml"),
            ),
            Item(
                "Driven **air-gapped**: a case in `crates/web/tests/air_gap.rs` "
                "(a listing the bundle does not carry is a different failure from a missing artifact)",
                needs=("crates/web/tests/air_gap.rs",),
            ),
            Item(
                "The credential boundary: a phase in `tests/heavy/authz.sh`, or `live:<kind>` "
                "when the kind has no local mode",
                needs=("tests/heavy/authz.sh", "tests/heavy/config.authz-live.toml"),
            ),
            Item(
                "A `<name>_publish_traversal_version_returns_400` regression test",
                needs=("crates/web/tests/*.rs",),
            ),
        ],
    ),
    Rule(
        title="A proxy handler changed",
        why="Two funnels validate coordinates in depth, but a handler that builds a "
        "storage key itself must still validate at the edge for a clean 400.",
        when=("crates/web/src/handlers/proxy/**",),
        items=[
            Item(
                "Any package name or version taken from the request goes through "
                "`validate_package_name`, and `..`/separators in the version are rejected, "
                "**before** it reaches a storage key"
            ),
            Item(
                "Every `200`/`201` in a `utoipa::path` declares `body = T` — "
                "`crates/web/tests/openapi_contract.rs` fails on any that does not"
            ),
            Item("A regression test in the relevant `crates/web/tests/*.rs`", needs=("crates/web/tests/*.rs",)),
        ],
    ),
    Rule(
        title="The API surface changed",
        why="The TypeScript client is generated, and the docs' API reference is generated "
        "from the same spec. A handler change that skips the resync ships a client that "
        "cannot call it.",
        when=("crates/web/src/lib.rs", "crates/web/src/handlers/**"),
        items=[
            Item(
                "`task dump-spec` then `task ui:generate`, with `ui/openapi.json` and "
                "`ui/src/client/` committed",
                needs=("ui/openapi.json", "ui/src/client/**"),
            ),
            Item("`ui/src/client/` was regenerated, never hand-edited"),
        ],
    ),
    Rule(
        title="A database migration was added",
        why="Migrations are embedded by a macro in a list, not discovered from the directory.",
        when=("crates/adapters/migrations/*.sql",),
        items=[
            Item(
                "A `mig!` entry in `embedded_migrator()` (`crates/adapters/src/migrations.rs`), "
                "with the sequence number incremented",
                needs=("crates/adapters/src/migrations.rs",),
            ),
            Item("The migration is forward-only and safe to apply to a live database"),
        ],
    ),
    Rule(
        title="Dependencies changed",
        why="Four security invariants live in the dependency tree and are enforced by "
        "`cargo-deny`; a bump can drag a banned crate back in silently.",
        when=("Cargo.toml", "Cargo.lock", "*/Cargo.toml", "crates/*/Cargo.toml"),
        items=[
            Item("`task security` is clean (cargo audit, cargo deny, pnpm audit, SBOM)"),
            Item(
                "Still out of the tree: `rsa`, `sqlx-mysql`/`sqlx-macros`, rustls 0.21, "
                "`h2 <0.4`, `lru <0.18.2` — and `actix-web`/`aws-sdk-s3` still have default "
                "features off"
            ),
            Item(
                "No new suppression: `advisories.ignore` stays `[]` in `deny.toml` and "
                "`.cargo/audit.toml` stays empty — fix or patch instead"
            ),
        ],
    ),
    # Four rules rather than one "generated files" rule: each fires on its own
    # source, so a diff that touched the roadmap is not handed three lines about
    # Helm and the RFC index. The first version did exactly that, and every item
    # it could not rule out carried a ⚠ — a warning that means nothing is worse
    # than no warning.
    Rule(
        title="Design tokens changed",
        why="`docs/.vitepress/theme/tokens.css` is generated from them and has a drift gate.",
        when=("ui/src/design/tokens.css",),
        items=[Item("`task ui:tokens`, with the generated copy committed", needs=("docs/.vitepress/theme/tokens.css",))],
    ),
    Rule(
        title="The roadmap changed",
        why="`ROADMAP.md` is canonical; the published page is generated from it and has a drift gate.",
        when=("ROADMAP.md",),
        items=[Item("`task docs:roadmap`, with `docs/guide/roadmap.md` committed", needs=("docs/guide/roadmap.md",))],
    ),
    Rule(
        title="Helm values changed",
        why="The chart README is generated by helm-docs and has a drift gate.",
        when=("helm/batlehub/values.yaml",),
        items=[
            Item(
                "A `# --` comment above each new value — that is how a value is documented, "
                "never by editing the README"
            ),
            Item("`task helm:docs`, with the regenerated README committed", needs=("helm/batlehub/README.md",)),
            Item("`task helm:lint` passes"),
        ],
    ),
    Rule(
        title="An RFC changed",
        why="The status banner, the `/rfc/` table and the sidebar are all generated from the "
        "RFC's own header table.",
        when=("docs/rfc/*.md",),
        unless_only=("docs/rfc/index.md",),
        items=[
            Item(
                "`Status`, `Short` and `Settles` are right in the RFC's header table — the "
                "listings are generated from those rows, so the RFC is what gets edited"
            ),
            Item(
                "`task rfc:index`, with `docs/rfc/index.md` and `docs/.vitepress/config.ts` committed",
                needs=("docs/rfc/index.md", "docs/.vitepress/config.ts"),
            ),
        ],
    ),
    Rule(
        title="Documentation changed",
        why="A page has exactly one home and one sidebar, and the French pages carry a "
        "freshness stamp naming the English revision they track.",
        when=("docs/**",),
        unless_only=("docs/rfc/**", "docs/internal/**"),
        items=[
            Item("`task docs:i18n:check` — a changed English page leaves its French copy stale"),
            Item("`task docs:links` and `task docs:structure` are clean"),
            Item("The page is in exactly one space and one sidebar (`task docs:audience`)"),
        ],
    ),
    Rule(
        title="The frontend changed",
        why="The Rust gates say nothing about the SPA, and the design gates are a "
        "separate workflow again.",
        when=("ui/**",),
        unless_only=("ui/src/client/**", "ui/openapi.json"),
        items=[
            Item("`task ui:lint`, `task ui:build` and `task ui:test` pass"),
            Item("`task ui:i18n:check` — a new string exists in every locale"),
            Item("The design gates pass at every viewport (`task ui:design`)"),
            Item("A label names what the control *does*, checked against the behaviour"),
        ],
    ),
    Rule(
        title="A heavy suite changed",
        why="A phase that passes because the client reached the upstream proves nothing.",
        when=("tests/heavy/**",),
        items=[
            Item("The phase was run locally, not only reasoned about"),
            Item("It asserts on the wire transcript (`heavy_wire_re_after`), not only on the client's exit code"),
            Item("Its ports do not collide with another suite's"),
        ],
    ),
    Rule(
        title="Rust changed",
        why="The per-PR gates, in the order they fail.",
        when=("crates/**", "server/**", "cli/**"),
        items=[
            Item("`cargo clippy --workspace -- -D warnings` and `cargo fmt --all --check`"),
            Item("`cargo test --workspace`"),
            Item("Line coverage stays at or above 80% (`task coverage-check`)"),
            Item(
                "`task fuzz:check` if a type a fuzz target constructs changed — `fuzz/` is a "
                "separate workspace and `--workspace` never compiles it"
            ),
        ],
    ),
]

# Shown when nothing above fires: the floor, not a guess.
FALLBACK = [
    "- [ ] The change does what the description says, and the description says what it does",
    "- [ ] Tests cover the behaviour that changed, not only the lines",
]


def build(paths: list[str]) -> str:
    fired = [r for r in RULES if r.fires(paths)]

    out = [MARKER, "## Before merging"]
    if not fired:
        out.append("")
        out.append(
            f"Nothing in these {len(paths)} changed file(s) matches a rule with "
            "obligations attached, so this is the floor:"
        )
        out.append("")
        out.extend(FALLBACK)
        out.append("")
        out.append(_footer(paths))
        return "\n".join(out) + "\n"

    total = sum(len(r.items) for r in fired)
    warnings = sum(
        1 for r in fired for i in r.items if i.needs and not matches(paths, *i.needs)
    )
    lead = f"{total} item(s) from the {len(paths)} file(s) this branch changed."
    if warnings:
        lead += (
            f" **{warnings}** marked ⚠ — the diff changed something that obliges them "
            "and not the file that discharges it."
        )
    out.append("")
    out.append(lead)

    for rule in fired:
        out.append("")
        out.append(f"### {rule.title}")
        if rule.why:
            out.append("")
            out.append(f"_{rule.why}_")
        out.append("")
        out.extend(item.render(paths) for item in rule.items)

    out.append("")
    out.append(_footer(paths))
    return "\n".join(out) + "\n"


def _footer(paths: list[str]) -> str:
    return (
        "<sub>Advisory — this check never fails a build. The rules live in "
        "`.github/scripts/pr_checklist.py`; a wrong or missing one is a four-line "
        "edit there. Ticking a box is a claim that a human made, which is the "
        "only kind of claim this list carries.</sub>"
    )


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--files", nargs="*", help="changed paths (default: read stdin)")
    args = ap.parse_args()

    if args.files is not None:
        paths = args.files
    else:
        paths = [line.strip() for line in sys.stdin if line.strip()]

    sys.stdout.write(build(paths))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
