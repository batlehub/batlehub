# Translating the documentation

The site publishes two trees. English is at the root and is canonical; French
is under `/fr/` and is a translation of it. This page says where a translation
lives, what a translator decides and what they must not, and — the part that
actually matters — what to do when you edit an English page that has already
been translated.

## Where a translation lives

`docs/fr/<same path as the English page>`. `docs/guide/caching.md` is translated at
`docs/fr/guide/caching.md`, and is served at `/fr/guide/caching`. Nothing else
changes: same filename, same heading structure, same section numbers.

Three spaces stay in English, and that is a decision rather than a backlog:

| Stays in English | Why |
| --- | --- |
| `contributing/` | Read by people changing the code, this page included. |
| `rfc/` | 305 000 words of design history. A record is not rewritten in a second language; it is quoted. |
| `docs/guide/roadmap.md` | Generated from `ROADMAP.md`, which is canonical. A French copy would be a second canonical roadmap that no gate could keep true. |

The French navigation links all three at their English URL. VitePress does not
fall back — a page that exists in English and not in French is a 404 under
`/fr/`, not a silent return to the English text — so a missing translation has
to be linked, not omitted.

## The two frontmatter keys

Every page under `docs/fr/` declares the page it translates and the revision it
was translated from:

```yaml
---
sourcePath: guide/caching.md
sourceHash: 3f1c9a2b7d4e5061
---
```

`sourceHash` is the first 16 hex characters of the SHA-256 of the English file.
`task docs:i18n:check` recomputes it and fails when the two disagree, which is
the whole mechanism: a translation cannot go stale quietly.

```bash
task docs:i18n:check    # gate — every French page is current with its source
task docs:i18n:status   # report — translated, stale, not started
task docs:i18n:stamp    # re-stamp, after bringing the French text up to date
```

## What to do when you edit an English page

The gate will go red on the next run whether or not you look, so the choice is
only about when. Three options, in order of preference:

1. **Update the French page too**, then `task docs:i18n:stamp` and commit both.
   Best for a change that is a paragraph or a table row.
2. **Leave the French page stale and say so in the pull request.** The gate is
   red, and that is the correct state: the site is publishing a French page that
   no longer matches the product. Someone has to fix it before the branch merges.
3. **Delete the French page** if the English one moved or was replaced outright.
   A deleted translation is honest; a translation of a page that no longer
   exists is not, and `docs:i18n:check` reports it as a source that is gone.

What is never right is running `task docs:i18n:stamp` on its own to make the
gate green. Stamping is the claim *"I have read what changed and the French text
now says it"*. Nothing can verify that claim, which is exactly why nothing does
it for you and why the stamp is a separate command.

## What a translator decides, and what they do not

The wording rules and the term table live in `docs/internal/glossaire-fr.md` —
unpublished, because it is a tool rather than a page. Two of its rules are worth
repeating here, because they are the ones that break a build rather than a
sentence:

- **Section numbers stay.** `## 6. Worked Examples` becomes `## 6. Exemples
  commentés`. `task docs:structure` reads those numbers, and an English page or
  an RFC that cites "§6" must land on the same section in both trees.

- **Generated blocks are not translated by hand.** The endpoint tables
  (`<!-- BEGIN endpoints: … -->`), the listing-filter table and the README
  support table are written by `task docs:endpoints`,
  `task docs:listing-coverage` and `task docs:readme-coverage`. The first
  translates its header row and keeps routes and summaries as the API declares
  them; the other two carry the English table into the French page, because
  their cells are prose generated from the Rust source and translating them
  would create a second source of truth for a sentence the code owns.

The vocabulary itself comes from `ui/src/locales/fr.json`, the console's own
translation. The console and the documentation have to say the same word for the
same thing, or a reader cannot connect the page to the button.

## Adding a page to the French tree

1. Copy the English page to `docs/fr/<path>` and translate it.
2. Add the two frontmatter keys, then `task docs:i18n:stamp` to fill the hash.
3. Add it to the French sidebar in `docs/.vitepress/nav/fr.ts`, in the group its
   English counterpart is in.
4. Run `task docs:design`. It carries the link, structure, audience and
   translation gates, and the French page is now in all four.

## See also

- [Working on BatleHub](/contributing/contributing) — the rest of the
  conventions a change follows.
