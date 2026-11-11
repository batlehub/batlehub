import { packagePath } from "@/config/navigation";
import type { ExploreEntryDto, UpstreamPackageDto } from "@/client/types.gen";
import type { ResolutionState } from "@/components/ui/resolution";

// ── Unified row type for the table ────────────────────────────────────────────

export type CachedRow = ExploreEntryDto & { kind: "cached" };
export type UpstreamRow = UpstreamPackageDto & { kind: "upstream" };
export type ExploreRow = CachedRow | UpstreamRow;

// ── Resolution as state ───────────────────────────────────────────────────────
//
// DESIGN.md's organising idea, and the reason this page exists in the form it
// does: what BatleHub holds and has verified renders at full resolution, what it
// does not renders coarse. The six states are named by the *server* — two of
// them need the registry's artifact TTL and the release-age gate's window and
// bypass roles, and `held` depends on who is asking — so this file's job is to
// map a string onto a mark and a word, not to decide anything.

const RESOLUTION_STATES = [
  "cached",
  "stale",
  "held",
  "pending",
  "yanked",
  "blocked",
] as const satisfies readonly ResolutionState[];

/** Spelled out, not `t(\`resolution.${s}\`)`: a template-literal key is invisible
    to the catalogue's reference gate, which is how 94 keys drifted out of use
    with everything green (RFC 0004-bis §2.2). */
export const STATE_LABEL_KEYS: Record<ResolutionState, string> = {
  cached: "resolution.cached",
  stale: "resolution.stale",
  held: "resolution.held",
  pending: "resolution.pending",
  yanked: "resolution.yanked",
  blocked: "resolution.blocked",
};

/**
 * The three states that are a refusal, and the rule behind each.
 *
 * The proof states a denial's rule *in its own row*, tied to the package by
 * `aria-describedby`, rather than floating one note above the table — so the
 * reason travels with the thing it is about, for a screen reader as well as for
 * a reader scanning the column.
 *
 * The wording names the mechanism and stops there. The proof's notes quote
 * specifics ("published 11 minutes ago, held until 24 h"); this listing does not
 * carry the block reason, the yank date or the quarantine's remaining window —
 * those live on the package's own page, which each note links to. Stating a
 * mechanism we know beats interpolating numbers we do not have.
 */
const NOTE_KEYS: Partial<Record<ResolutionState, string>> = {
  blocked: "resolution.noteBlocked",
  yanked: "resolution.noteYanked",
  held: "resolution.noteHeld",
};

/** An upstream search hit has never been fetched, which is `pending`'s
    definition; a cached row carries whatever the server graded it. */
export function rowState(row: ExploreRow): ResolutionState {
  if (row.kind !== "cached") return "pending";
  return (RESOLUTION_STATES as readonly string[]).includes(row.state)
    ? (row.state as ResolutionState)
    : "pending";
}

export function noteKey(row: ExploreRow): string | undefined {
  return NOTE_KEYS[rowState(row)];
}

/**
 * Why this row is here, or `undefined` when it needs no saying.
 *
 * Only a prose match is labelled. A row that matched on its name is
 * self-explanatory, and labelling every row would make the one that actually
 * needs the label invisible (RFC 0007-bis §4.3).
 */
export function matchLabel(t: (key: string) => string, row: ExploreRow): string | undefined {
  if (row.kind !== "cached") return undefined;
  const matched = (row as ExploreEntryDto).matched_in;
  if (matched === "readme") return t("packageCatalog.matchedReadme");
  if (matched === "both") return t("packageCatalog.matchedBoth");
  return undefined;
}

/** The matched fragment of a README, as plain text. Never markup. */
export function rowSnippet(row: ExploreRow): string | undefined {
  if (row.kind !== "cached") return undefined;
  return (row as ExploreEntryDto).snippet ?? undefined;
}

/** Stable across re-renders because it is derived from the row's identity, not
    from its index: `aria-describedby` pointing at a recycled id is worse than
    pointing at nothing. */
export function rowId(row: ExploreRow): string {
  return `${row.kind}-${row.registry}/${row.name}`;
}

/**
 * Both kinds of row lead to the same page, and the upstream one used to lead
 * nowhere.
 *
 * `if (row.kind !== "cached") return` made a package this instance has not
 * pulled the one thing in the catalog you could not open — while the page it
 * would have opened was already built for exactly that case: `explore/detail.rs`
 * calls `upstream_detail` for anything not held here, and `PackageDetailPage`
 * renders the result under "N version(s) below exist upstream and are not held
 * here", with the per-version **Fetch this version** button RFC 0007-bis §6.4
 * put there. Measured against the running instance, `npm/chalk` — never pulled —
 * answers with 44 versions, their publication dates and their firewall verdicts.
 * So the capability was reachable only by typing the URL, which is not a
 * capability the console had.
 *
 * `UpstreamPackageDto` carries `registry` and `name`, the same two fields the
 * cached row uses, so there is nothing to resolve first: deciding *before* the
 * click whether a package deserves a page is the check that was wrong, not the
 * URL it built.
 *
 * A path rather than a handler, because the row used to be a `@click` on
 * `<TableRow>` with no `tabindex`, no `role` and no key handler, and the
 * package name was plain text rather than a link. The only keyboard route to a
 * package's page was the **details** link *inside a refusal note*, which
 * renders only when `noteKey(row)` is set — so for a normally cached package,
 * a keyboard user could not open its detail page at all. That is the product's
 * primary navigation path, and WCAG 2.1.1 Keyboard (A) is the floor it missed.
 *
 * The link navigates with a **push**, not a `replace`. The version list on the
 * detail page replaces, for the reason stated there; this one must not.
 * `goBack()` restores the catalog's search, scope, sort, page and scroll offset
 * only when `history.state.back` is `/packages`, and replacing the catalog
 * entry on the way in is exactly what would stop it being that.
 */
export function detailPath(row: ExploreRow): string {
  return packagePath(row.registry, row.name);
}
