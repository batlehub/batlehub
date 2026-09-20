---
name: BatleHub
description: A bitmap type specimen for an artifact proxy, in two renditions, where an artifact's state is its resolution.
colors:
  ground: "oklch(0.07 0.018 18)"
  ground-raised: "oklch(0.155 0.022 18)"
  ground-sunk: "oklch(0.045 0.014 18)"
  ink: "oklch(0.93 0.018 25)"
  ink-dim: "oklch(0.62 0.045 30)"
  rule-soft: "oklch(0.34 0.03 25)"
  rule-strong: "oklch(0.52 0.06 25)"
  accent: "oklch(0.65 0.235 25)"
  accent-ink: "oklch(0.10 0.02 18)"
  copper: "oklch(0.72 0.14 52)"
  focus: "oklch(0.85 0.16 85)"
  light-ground: "oklch(0.97 0.008 18)"
  light-ground-raised: "oklch(0.935 0.010 18)"
  light-ground-sunk: "oklch(0.99 0.004 18)"
  light-ink: "oklch(0.20 0.022 20)"
  light-ink-dim: "oklch(0.44 0.04 25)"
  light-rule-soft: "oklch(0.80 0.02 25)"
  light-rule-strong: "oklch(0.62 0.05 25)"
  light-accent: "oklch(0.52 0.21 25)"
  light-accent-ink: "oklch(0.99 0.004 18)"
  light-copper: "oklch(0.50 0.12 52)"
  light-focus: "oklch(0.55 0.11 85)"
typography:
  # The Silkscreen ramp, enumerated. The named roles below carry one size each,
  # which is enough for a face with one size — but this one is drawn on an 8px
  # em and every legal size is a multiple of it, so `display` alone declares 56
  # and leaves 40 / 72 / 88 / 104 looking undeclared to anything reading this
  # file. The console never exposes them as literals (`var(--t-display)` steps
  # per breakpoint); the documentation site's hero writes two of them directly,
  # which is what surfaced the gap. Keys are the multiplier, because that is the
  # thing The Integer Em Rule is actually about.
  scale:
    silkscreen-2x: "16px"
    silkscreen-3x: "24px"
    silkscreen-5x: "40px"
    silkscreen-7x: "56px"
    silkscreen-9x: "72px"
    silkscreen-11x: "88px"
    silkscreen-13x: "104px"
  display:
    fontFamily: "Silkscreen, monospace"
    fontSize: "56px"
    fontWeight: 700
    lineHeight: 0.92
    letterSpacing: "0.02em"
  pixel-md:
    fontFamily: "Silkscreen, monospace"
    fontSize: "24px"
    fontWeight: 700
    lineHeight: 1.6
    letterSpacing: "0.04em"
  pixel-sm:
    fontFamily: "Silkscreen, monospace"
    fontSize: "16px"
    fontWeight: 400
    lineHeight: 1.6
    letterSpacing: "0.04em"
  head:
    fontFamily: "JetBrains Mono, ui-monospace, monospace"
    fontSize: "20px"
    fontWeight: 400
    lineHeight: 1.6
    letterSpacing: "normal"
  sub:
    fontFamily: "JetBrains Mono, ui-monospace, monospace"
    fontSize: "16px"
    fontWeight: 400
    lineHeight: 1.6
    letterSpacing: "normal"
  row:
    fontFamily: "JetBrains Mono, ui-monospace, monospace"
    fontSize: "15px"
    fontWeight: 400
    lineHeight: 1.6
    letterSpacing: "normal"
  body:
    fontFamily: "JetBrains Mono, ui-monospace, monospace"
    fontSize: "13px"
    fontWeight: 400
    lineHeight: 1.6
    letterSpacing: "normal"
  meta:
    fontFamily: "JetBrains Mono, ui-monospace, monospace"
    fontSize: "12px"
    fontWeight: 400
    lineHeight: 1.6
    letterSpacing: "0.1em"
rounded:
  none: "0px"
spacing:
  s1: "4px"
  s2: "8px"
  s3: "12px"
  s4: "16px"
  s5: "24px"
  s6: "40px"
components:
  button-action:
    backgroundColor: "{colors.accent}"
    textColor: "{colors.accent-ink}"
    typography: "{typography.pixel-sm}"
    rounded: "{rounded.none}"
    padding: "12px 16px"
  button-action-disabled:
    backgroundColor: "transparent"
    textColor: "{colors.ink-dim}"
    typography: "{typography.pixel-sm}"
    rounded: "{rounded.none}"
    padding: "12px 16px"
  button-ctl:
    backgroundColor: "transparent"
    textColor: "{colors.ink-dim}"
    typography: "{typography.meta}"
    rounded: "{rounded.none}"
    padding: "8px 12px"
  button-ctl-hover:
    backgroundColor: "transparent"
    textColor: "{colors.ink}"
    typography: "{typography.meta}"
    rounded: "{rounded.none}"
    padding: "8px 12px"
  input-search:
    backgroundColor: "{colors.ground-sunk}"
    textColor: "{colors.ink}"
    typography: "{typography.row}"
    rounded: "{rounded.none}"
    padding: "12px"
  nav-link:
    backgroundColor: "transparent"
    textColor: "{colors.ink-dim}"
    typography: "{typography.meta}"
    rounded: "{rounded.none}"
    padding: "8px 12px"
  nav-link-current:
    backgroundColor: "{colors.ink}"
    textColor: "{colors.ground}"
    typography: "{typography.meta}"
    rounded: "{rounded.none}"
    padding: "8px 12px"
  segment-cell:
    backgroundColor: "transparent"
    textColor: "{colors.ink-dim}"
    typography: "{typography.meta}"
    rounded: "{rounded.none}"
    padding: "8px 4px"
  segment-cell-current:
    backgroundColor: "{colors.ink}"
    textColor: "{colors.ground}"
    typography: "{typography.meta}"
    rounded: "{rounded.none}"
    padding: "8px 4px"
  popover:
    backgroundColor: "{colors.ground-sunk}"
    textColor: "{colors.ink}"
    typography: "{typography.body}"
    rounded: "{rounded.none}"
    padding: "16px"
    width: "264px"
  bay-item:
    backgroundColor: "transparent"
    textColor: "{colors.ink-dim}"
    typography: "{typography.row}"
    rounded: "{rounded.none}"
    padding: "8px 24px"
  bay-item-current:
    backgroundColor: "{colors.ground-raised}"
    textColor: "{colors.ink}"
    typography: "{typography.row}"
    rounded: "{rounded.none}"
    padding: "8px 24px"
  table-cell:
    backgroundColor: "transparent"
    textColor: "{colors.ink}"
    typography: "{typography.row}"
    rounded: "{rounded.none}"
    padding: "12px 12px 12px 0"
  table-row-hover:
    backgroundColor: "{colors.ground-raised}"
    textColor: "{colors.ink}"
    typography: "{typography.row}"
    rounded: "{rounded.none}"
    padding: "12px 12px 12px 0"
  panel:
    backgroundColor: "transparent"
    textColor: "{colors.ink-dim}"
    typography: "{typography.body}"
    rounded: "{rounded.none}"
    padding: "40px 24px"
  panel-error:
    backgroundColor: "transparent"
    textColor: "{colors.accent}"
    typography: "{typography.body}"
    rounded: "{rounded.none}"
    padding: "40px 24px"
---

# Design System: BatleHub

Recorded from the built proving surface at `ui/design-proof/index.html` (plus its authored
`halftone-plate.png` and self-hosted faces). That build is the ground truth for every value below;
where it diverges from the RFC's earlier intent, the build wins and the divergence is noted.
`ui/src` has not been migrated yet — Phase 2 derives `ui/src/design/tokens.css` from this file, **now
for two renditions.**

**Verification status.** No browser can execute in the build container, so nothing here is
automatically verified. The human author has since opened the built surface in a real browser and
confirmed **both renditions**, and that viewing produced one real defect, since fixed: horizontal
overflow of the whole page at small widths, caused by fixed table column widths. Still unverified:
the plate as composited, reflow at 320–480px under the longer French labels, focus rings as painted,
whether the `woff2` files decode, and whether `filter: blur()` on `<tr>` animates cleanly. The
pager's Next control remains drawn but unwired. Contrast ratios below were computed, not sampled.

**On the frontmatter's two colour sets.** Token *names* are rendition-independent: a component asks
for `--accent`, never for a rendition. The unprefixed frontmatter values are the `:root` declarations
(the dark rendition, which is also the fallback if the resolve script fails); the `light-*` values are
the `:root[data-theme="light"]` overrides. There is no third set and no per-component override.

## Overview

**Creative North Star: "The Proof Sheet"**

A press proof. One form is set at poster scale and captioned by its facts; everything under it is
ruled rows, hairlines and ink. The world is an Emigre bitmap type specimen: a square-pixel display
face carrying identity and scale, a mono text face carrying every fact, and an authored halftone
plate as the only image on the page. There is no photography, no illustration, no card, no tile, and
no glow.

The organising idea is **resolution as state**. What BatleHub holds and has verified renders at full
resolution — a fine 3×3 dot matrix, ink-white; what it does not renders coarse — a 2×2 matrix at
larger cells, in copper, dim or crimson. The single authored motion in the system is a row resolving
from blurred and low-contrast to crisp, which is what a cache miss becoming a hit actually looks
like. State is never carried by hue alone: pattern, word and hue all say it.

**The system has two renditions, and paper is the home ground.** The specimen board this world comes
from is ink on paper, so the light rendition is the board itself, not a filter laid over a dark
design. The dark rendition is the *adaptation the use scene earned*: a shell in a dark room, one tab
among twenty, opened in anger, often left on a second screen as ambient status. Both are first-class,
both are measured, and the dark set is the base declaration only because that scene is the confirmed
one. Which rendition a reader gets is a stored preference — System / Light / Dark — resolved before
first paint, alongside the identically-shaped System / English / Français locale preference.

Density follows from the same scene: this is a scanning surface, tuned for a reader who arrived with
a question after a build failed, and it earns its calm by refusing decoration rather than by adding
whitespace. The palette is the Monofolio OKLCH line in both renditions, kept because measurement said
it was also the right palette for this world.

**Key Characteristics:**

- Two renditions of one palette — warm near-black and warm paper — with one crimson accent, copper
  second, and everything else ink and rule.
- Two type ramps: a square-pixel display face on an 8px grid, and a mono text face for all data.
- Hairline rules, dashed at rest and solid under state; no cards, no tiles, no radius, no shadow at rest.
- State reads as resolution — fine dots for held-and-verified, coarse dots for not — and the state
  grammar is identical in both renditions.
- Every denial states its own rule, in its own row, tied to the artifact it refused.
- One authored raster (a 60 lpi halftone plate) is the only image in the system; it is the ink of
  whichever ground it sits on.
- Theme and locale are stored as *preferences*, never as resolved values, and share one control.

## Colors

One palette, two grounds. Each rendition carries a ground, warm ink, one synthetic crimson used
sparingly, and a copper second voice for anything that is *waiting* rather than *refused*.

**The renditions are the same system, not two systems.** The evidence is in the build: apart from the
token block itself, the entire stylesheet contains exactly **one** rendition-specific rule — the
plate's opacity and fill. No component, no state, no border and no type step is re-specified for
light. That is possible because the state grammar never depended on fill in the first place (see The
Undependable Fill Rule), so nothing had to be re-invented when the ground flipped.

**Direction, not lightness, is what the neutral names mean.** `--ground-raised` moves toward the ink
pole and `--ground-sunk` away from it, so on near-black raised is *lighter* (L .155 over L .07) and on
paper raised is *darker* (L .935 under L .97). A token is named for the job it does, and each
rendition moves it toward its own opposite pole.

### Primary

- **Signal Crimson** — `--accent`, the one synthetic colour in the world. `#ff343d` as painted on
  near-black at **5.76:1**; `#c50220` as painted on paper at **5.63:1**. Load-bearing exactly four
  ways and no more: link text, the fill under counter-ink on the one primary action, the `blocked`
  state, and the 1px lit edge on the selected registry. Both renditions' authored values are clamped
  to the sRGB gamut — see The In-Gamut Rule, which this token has now confirmed twice.
- **Counter-Ink** — `--accent-ink`, the foreground that rides on a crimson fill and nowhere else
  (plus the near edge of the pixel step on `:active`). Near-black at chroma 0.02 in the dark
  rendition, measuring **5.69:1** on the fill; near-white at chroma 0.004 in the light rendition,
  measuring **5.97:1** on its darker crimson. Never a background.

### Secondary

- **Aged Copper** — `--copper`, **8.06:1** on near-black and **5.74:1** on paper. The second voice,
  and it means *pending or held*, never *good*: the `stale` and `held` states, the instance hostname
  in the identity strip, the pressed border on a toggled control, the "synthetic fixture data" tag,
  a measured value moving the wrong way but not yet refused — a falling hit rate, a quota approaching
  its limit — and the plate's ink in the dark rendition. It is the only colour that appears as a large
  area anywhere in the system, and only through the plate. Its lightness is re-derived per ground —
  see The Re-Derived Lightness Rule.

  The degradation job was added after two surfaces reached for it independently — `AdminDashboard`'s
  falling-hit-rate trend and `QuotaWidget`'s warning state — with no entry authorising either. Two
  surfaces improvising the same absent thing is the shape of a job the system needs, and The One
  Synthetic Rule caps the palette, so it is a job copper gains rather than a sixth hue. What stays
  true is the negative half: copper never means *good*. What is no longer exhaustive is *pending or
  held* — a worsening metric is neither, and it is still not a refusal, which is the distinction the
  hue exists to carry. See RFC 0004-bis §11/O1.

### Tertiary

- **Signal Amber** — `--focus`, **13.07:1** on near-black and **4.48:1** on paper. Reserved entirely
  for the focus ring (`2px solid`, `2px` offset). It appears nowhere else, at no other size, for no
  other reason. On paper it is a darker amber than on black for the same reason copper is: the hue
  survives the crossing, the lightness does not.

### Neutral

- **Ground** — `--ground`, the page. Warm near-black at hue 18 in the dark rendition, warm paper at
  the same hue in the light one. Neither is neutral grey, and neither is `#000` or `#fff`.
- **Ground Sunk** — `--ground-sunk`, the masthead, the settings popover, the search field's well and
  the fixture-data footer. Computes to **1.005:1** against near-black; on paper it moves the other
  way (L .99 over L .97) and is no more dependable there.
- **Ground Raised** — `--ground-raised`, row hover and the selected registry cell. **1.06:1** against
  near-black, **1.11:1** against paper. Confirmation, never elevation.
- **Ink** — `--ink`, **16.88:1** on near-black and **16.64:1** on paper. Package names, headings,
  display type, and the reversed foreground inside any active segment cell.
- **Dim Ink** — `--ink-dim`, **5.62:1** on near-black (**5.28:1** on raised) and **7.24:1** on paper
  (**6.52:1** on raised). Every label, column head, caption, count, timestamp and secondary control.
  It is the floor for small text in both renditions; nothing smaller than 15px is ever painted in a
  lighter value than this.
- **Soft Rule** — `--rule-soft`. Separators only — section edges, the dashed row divider, the dot
  field. Never a control edge; it is not a contrast-carrying value.
- **Strong Rule** — `--rule-strong`, **3.68:1** on near-black (**3.46:1** on raised, **3.70:1** on
  sunk) and **3.41:1** on paper. Every interactive boundary: control borders, the nav and segmented
  frames, the table head rule, the hovered row's divider, and the note's tie bar.

### Named Rules

**The In-Gamut Rule.** Every colour token is authored in-gamut for sRGB *as written*, never caveated
in prose, with the Monofolio source value kept as a provenance comment rather than as the computed
value. The rule now has **two confirmed cases in one palette**, which is why it is law and not a
one-off correction. Dark `--primary` ships `oklch(0.65 0.26 25)`; the sRGB maximum at that lightness
and hue is **0.2359**. Light `--primary` ships `oklch(0.52 0.24 25)`; the maximum there is
**0.2108**. Both are outside the gamut, both are clamped in the built world, and **both measure
better clamped than the engine-dependent original** — a token that leaves the gamut cannot carry a
contrast guarantee, because engines then disagree on what they paint (naive clipping and CSS Color 4
chroma reduction land on different colours with different ratios). If wide-gamut is wanted later it
layers as an `@supports (color: color(display-p3 …))` enhancement over the in-gamut base, with AA
measured against the base. (RFC 0003 R11.)

**The Undependable Fill Rule.** No fill step separates two surfaces, **in either rendition**:
`--ground-raised` is 1.06:1 against near-black and 1.11:1 against paper, because the WCAG contrast
ratio compresses at *both* ends of the lightness range, not only the dark end. Neither value is a
dependable surface, and neither may ever be the *only* signal for a state or a boundary. State is
carried by rule weight, ink and the dot field; fill is a secondary cue that confirms what another
channel already said. **This is the reason the two-rendition system is coherent rather than
duplicated:** a grammar that never leaned on fill transfers between grounds unchanged, so the
component layer needed no light-specific rules at all. Both tokens stay declared — they do real work
as confirmation — but they are not elevation, and a future rendition inherits the same prohibition.

**The Re-Derived Lightness Rule.** When a token crosses renditions, **the hue relationship is what
survives; the lightness is re-derived against the new ground.** Copper is the worked example:
Monofolio's light copper sits at L .58 and measures 4.2:1 on paper, under the body-text floor, so it
darkens to L .50 for **5.74:1** — same hue 52, same role, re-measured lightness. Amber focus does the
same, L .85 → L .55. Never port a lightness across a ground and never assume a ratio survives the
crossing; port the hue, then re-derive L and re-measure every pair the token participates in.

**The Counter-Ink Rule.** A crimson fill always takes `--accent-ink`, and `--accent-ink` is whichever
pole clears AA against that rendition's crimson — dark ink on the light crimson (5.69:1), paper ink on
the dark crimson (5.97:1). It is never "white on the accent" by habit: the incumbent
`--primary-foreground` on `--primary` measured 3.58:1 and failed AA on every filled button in the
current UI. Do not re-inherit the incumbent pairing when porting a Monofolio component, and do not
assume the counter-ink keeps its polarity across renditions.

**The One Synthetic Rule.** Crimson is the world's only invented colour and stays on its four jobs.
Copper carries "waiting"; ink carries "known"; dim ink carries everything ordinary. A fifth hue does
not get added to signal a fifth condition — the dot pattern does that.

## Typography

**Display Font:** Silkscreen 400/700 (self-hosted `woff2`, fallback `monospace`)
**Body / Data Font:** JetBrains Mono, variable weight 100–800 (self-hosted `woff2`, fallback
`ui-monospace, monospace`)

**Character:** A square-pixel bitmap face for identity and scale against a precise, humane mono for
every fact on the page. The pairing is a specimen sheet's: one form shown large enough to see how it
is drawn, and a caption set small enough to be read. Both faces are self-hosted and preloaded, with
`font-display: swap` — a blank masthead on a cold cache is worse than a reflow. Neither ramp changes
between renditions.

**There are two ramps because Silkscreen is drawn on an 8px em.** Its square pixel only stays square
at integer multiples of 8, so every Silkscreen size is one: 16 / 24 / **40** / 56 / 72 / 88 / 104 px
(2× / 3× / **5×** / 7× / 9× / 11× / 13×). 5× is the documentation site's hero step below 640px and
the only size on this list that is not also a `--t-display` breakpoint: the face's advance measures
0.848em and it does not hyphenate, so "BatleHub" needs 380px at 56px against a 336px column and
breaks between glyphs. The rule the face is actually governed by is the integer em, not the
console's four breakpoints, which are a property of a full-bleed specimen head. (RFC 0005 §13.)
JetBrains Mono carries no such constraint and uses a conventional
20 / 16 / 15 / 13 / 12 ramp.

### Hierarchy

- **Display** (Silkscreen 700, 56px → 72 discrete per breakpoint, line-height 0.92, 0.02em): the
  page-title step, and the one form set at poster scale — the subject at the top of the sheet, one
  per view. Every primary destination sets its title here: the home greeting, the catalog's
  registry, the package sheet's package, the setup guide. Uppercase where the subject has no case
  of its own (the catalog's registry name); not where it does (a package name, a `user_id`), since
  Silkscreen is caps-only and the transform would only restate the face.
  It was 56 → 72 → 88 → 104, authored for the catalog specimen alone; 104px costs two lines to any
  title that is not one short word, which is what made it a per-page decision instead of a step.
- **Pixel Medium** (Silkscreen 700, 24px, 0.04em): the `BatleHub.` wordmark only.
- **Pixel Small** (Silkscreen, 16px): pixel-scale labels inside components — the registry name in a
  list row (0.02em), the primary action's label (0.04em, uppercase), a panel heading, the settings
  popover's own heading, a page number. This is the size at which the bitmap face is still a label
  and not yet an image.
- **Head** (JetBrains Mono, 20px): declared in the ramp; the section-title step. Still unset by a
  shipped surface — treat its usage as open.
- **Reading** (JetBrains Mono, 16px — the Sub step — line-height 1.7, max 68ch): long-form prose, on
  the documentation site. This is the one role authored for *reading for minutes* rather than
  scanning for seconds, and it is the step the ramp had been holding open. See The Reading Role Rule
  below for why its leading is not the system's usual 1.6.
- **Row** (JetBrains Mono, 15px, line-height 1.6): the scanning size — package names, list rows,
  search input. The densest text a reader is expected to read every line of.
- **Body** (JetBrains Mono, 13px, line-height 1.6): prose — the specimen caption (max 72ch), denial
  notes, panel copy (max 64ch), the pager.
- **Meta** (JetBrains Mono, 12px, uppercase, tracked): labels and chrome — column heads, nav items,
  segment cells, state words, counts, controls, preference labels and notes, the identity strip.
  Always `--ink-dim` or better; never below 12px.

### Named Rules

**The Integer Em Rule.** Silkscreen only ever appears at a multiple of 8px. `--t-display` is
discrete per breakpoint (56 / 72 / 88 / 104 at 640 / 880 / 1140px) rather than a `clamp()` for
exactly this reason: a fluid 10vw is an integer multiple at only a handful of viewport widths, which
left the focal element off its own pixel grid across the whole 560–1040px band. Never fluid-size the
bitmap face.

**The Tracking Ladder Rule.** Uppercase mono is tracked, and the amount encodes how far the label is
from its content: 0.06em inside a segment cell (where the label *is* its own content and the cell is
narrow), 0.10em for inline chrome and state words, 0.14em for table column heads and preference
labels, 0.16em for a standalone section label. Lowercase text is never tracked.

**The Reading Role Rule.** Prose read for minutes takes 16px at **line-height 1.7 and a 68ch
measure**, not the 13px/1.6 the console spends on a caption. Three things compound to earn the extra
leading, and none of them applies to a list row: a 67-character line is long, light ink on a dark
ground needs compensation, and a monospace face offers no word-shape cue for the return sweep.
**Tracking is not part of that compensation** — the Tracking Ladder Rule ends at "lowercase text is
never tracked", and it wins; the leading carries it alone. Weight stays 400, because weight
compensation answers low contrast and ink measures 16.88:1 here. Measured, not estimated: JetBrains
Mono's advance is exactly 0.6em (9.0px at 15px, 9.6px at 16px), so `ch` is a true character count in
this world and the 45–75 band applies literally. The three candidates and why 16/1.7/68 won are
recorded in the documentation site's surface brief.

**The Data Face Rule.** Every number a reader might compare — counts, sizes, versions, page numbers
— is `font-variant-numeric: tabular-nums`, right-aligned when it sits in a column, and formatted
through one locale formatter shared by the whole surface, keyed off the resolved locale. Never
letterspace a number.

## Layout

**The sheet.** Full-bleed; there is no centred max-width container. The page is a masthead, a
specimen head, and then a two-column sheet: a 232px registry bay (sticky to the top of the viewport)
and a fluid catalog column that is allowed to shrink (`min-width: 0`) so the table controls its own
overflow.

**Rhythm.** One 4px-based scale, six steps: 4 / 8 / 12 / 16 / 24 / 40. 12px is the inside of a
control, 16px the inside of a popover, 24px the inside of a region, 40px the top of the specimen and
the inside of an empty-state panel. There is no 32px step and no half-steps; a value not on the scale
is a defect.

**Measure.** Prose is capped even though the sheet is not: the specimen caption at 72ch, panel copy
at 64ch. Table cells are unconstrained, but package names and denial notes wrap at any point
(`overflow-wrap: anywhere`), because the names are `@scope/pkg` and
`org.springframework:spring-core`, not sentences. Use `overflow-wrap: anywhere`, never
`word-break: break-word` — that value is deprecated and several engines treat it as `normal` for
overflow purposes, which is exactly the case that matters.

**Responsive.** Four breakpoints, and only one of them changes structure:

- **640 / 880 / 1140px** — display size steps only (72 / 88 / 104px). Nothing else moves.
- **900px and below** — the sheet collapses to one column; the bay unsticks and becomes a
  horizontally scrolling row of registry chips with its selection edge moving from the left border to
  a 2px bottom border; region padding drops 24 → 16px; the plate widens to 64% and drops to 26%
  opacity; the two least load-bearing columns (Size, Last fetch) are removed from the table rather
  than being squeezed; the remaining fixed column widths release to `width: auto`; and the state chip
  is allowed to wrap.

**Column dropping, not squeezing.** When width runs out, whole columns leave the table. State,
package and version never leave — they are the answer to the question the reader arrived with. The
columns that stay are sized by class (`.c-state`, `.c-ver`, `.c-size`, `.c-fetch`), and those classes
release to `width: auto` below 900px: at 390px the fixed widths alone claimed 260px of a 358px box.

### Named Rules

**The No-Container Rule.** Regions are separated by full-width hairline rules, not by an inset
container with a background. A section's edge is a line across the whole sheet; the page never grows
a visible box around its content.

**The Own-Container Overflow Rule.** Wide content scrolls inside its own container; **the body never
scrolls horizontally.** The table lives in a `.table-wrap` with `overflow-x: auto` and
`overscroll-behavior-x: contain`, and every full-width region is clamped to `max-width: 100%` at the
collapse breakpoint. This is the one defect a real browser caught — the whole page slid sideways at
small widths — so it is recorded as law rather than as a fix.

## Elevation & Depth

**This system has no elevation.** There are no ambient shadows, no blurs, no layered surfaces, and
no glow — the Monofolio `--cyber-glow` / `--steam-glow` utilities do not survive into this world.
Depth is entirely inked: a hairline changes weight, a value changes, or a texture appears. The two
tonal steps that exist (`--ground-raised`, `--ground-sunk`) are confirmation, not elevation, and
cannot be seen on their own in either rendition (see The Undependable Fill Rule). The settings
popover is no exception: it is a 1px-framed box on `--ground-sunk`, floated by `z-index` alone, with
no shadow separating it from the sheet.

Two hard-edged offsets exist, and both are the bitmap world's own material rather than lighting —
zero blur, solid colour, on the primary action only:

### Shadow Vocabulary

- **Action ring** (`box-shadow: 0 0 0 2px var(--ground), 0 0 0 3px var(--accent)`): hover on the one
  primary action. A cut-out ring, not a halo. It reads in both renditions because its inner band is
  the ground token itself.
- **Pixel step** (`box-shadow: 3px 0 0 var(--accent-ink), 6px 0 0 var(--accent)` with
  `transform: translateX(-2px)`): `:active` only. The button displaces sideways and leaves two
  stacked plates behind it, like a mis-registered print pull.

### The material layer

**The plate** is the system's one image: an authored raster (`halftone-plate.png`), a 45° halftone
screen at 60 lpi generated deterministically, running **99.7% ink coverage down to 6.3%** across its
width, emitted as 8-bit grayscale + alpha. It is applied as a CSS **mask** over a colour fill, so the
ink stays a token rather than being baked into the file. It sits behind the specimen head at
`min(52%, 620px)` wide, anchored right, `pointer-events: none`, `aria-hidden`.

**The same authored raster serves both renditions, inked with its own ground's colour**: `--copper`
at **34%** opacity on near-black (26% below 900px), `--ink` at **20%** on paper — lower, because dark
dots on a light ground carry further. Same screen, same geometry, the ink of its own ground. Nothing
else about the plate changes.

Two rules the build encodes and Phase 2 must keep: the plate is **mirrored** (`scaleX(-1)`) so its
dense end bleeds off the outer margin instead of facing the caption — copper at 34% over near-black
composites to roughly `#513021`, where `--ink-dim` would fall to 3.21:1; and its fill is
**transparent until masking is confirmed supported**, in both renditions.

**The dot field** is the second texture: a 9×9px radial dot grid in `--rule-soft` at 55% opacity,
gradient-masked to fade at both ends. It lives only in the masthead's flex spacer — the one box that
holds no text and cannot acquire any, and which collapses to zero width exactly when the bar wraps.
Measured behind wrapped 12px `--ink-dim`, the field drops that text to 4.50:1, so it is confined by
construction rather than tuned by opacity.

### Named Rules

**The Flat-At-Rest Rule.** Nothing casts a shadow at rest. The only two box-shadows in the system are
hard-edged, zero-blur, and belong to `:hover` and `:active` on the primary action. A soft or offset
shadow anywhere else — cards, panels, popovers, the masthead — is out of the world.

**The Texture-In-Empty-Boxes Rule.** A texture may only occupy a box that holds no text and cannot
come to hold text. Painting order is not contrast: if a field can end up behind a label, it is
confined, not faded.

**The Gated Material Rule.** A masked element's fill is declared **only inside the `@supports` query
that confirms the mask**, per rendition. An engine or minifier that dropped the mask would otherwise
paint a solid copper (or ink) block over the right half of the specimen — precisely the 3.21:1
condition the whole treatment exists to avoid. A material's failure mode must be *absence*, never a
solid block.

## Shapes

**Zero radius, everywhere.** Every corner in the system is square — buttons, inputs, panels, the nav
frame, the segmented groups, the settings popover, the state chips. Monofolio's incumbent 2px radius
does not survive into this world; the square corner is the bitmap face's own geometry, and rounding
it re-introduces a different world's softness.

**Everything is 1px.** Rules, control borders, region edges, segment dividers, the note's tie bar and
the selected registry's lit edge are all exactly 1px. The only 2px strokes in the system are the
focus ring and the mobile bay's selected underline. There is no 3px or thicker border, and no
coloured side stripe — a thick accent bar on a row is refused; the 1px lit edge plus ink does that
job.

**Dashed means "at rest", solid means "engaged".** Row dividers are `1px dashed var(--rule-soft)`.
On hover the same divider becomes `1px solid var(--rule-strong)`. This is the replacement for the
retired decorative grid: the edge survives, but every line now separates two real things.

**One bounding box, internal dividers, no gaps.** Any group of mutually exclusive choices — the
primary nav, the language preference, the theme preference — is drawn as a single 1px frame with 1px
cell dividers and `gap: 0`. Adjacent controls that are *not* a choice set keep the 12px toolbar gap
and their own separate frames.

**The dot grid is the icon system.** The wordmark is a 3×3 grid of 4px squares on a 16px viewBox with
2px gutters, and the state matrices are the same figure at two resolutions. Icons are inline SVG at
14–16px with 1.5px strokes; there is no icon font and no glyph-as-icon.

## Components

Buttons, fields and rows are all built the same way: a 1px `--rule-strong` boundary, a square corner,
uppercase tracked mono, and no fill unless the control is the one action on the view. Every component
below is written once and renders in both renditions unchanged.

### Buttons

- **Shape:** square (0 radius), 1px boundary.
- **Primary — the one filled action** (`.action`): crimson fill under counter-ink, Pixel Small
  uppercase, 12/16px padding. Exactly one per view. Hover paints the cut-out ring; `:active` fires
  the pixel step; disabled drops the fill entirely and becomes a dim outlined control (crimson never
  appears in a disabled state).
- **Secondary — the control** (`.ctl`): transparent, 1px `--rule-strong`, `--ink-dim` Meta uppercase,
  8/12px padding. Hover lifts the text to `--ink` and the border to `--ink-dim`. `aria-pressed="true"`
  lifts the text to `--ink` and turns the border copper. Disabled is `opacity: .5`.
- **Focus:** every control shows the shared amber ring (`2px solid var(--focus)`, 2px offset). Focus
  is never suppressed, never replaced by a colour change, and never inherited from the browser
  default.

### Inputs / Fields

- **Style:** a well, not a box — `--ground-sunk` behind a 1px `--rule-strong` border, 12px padding,
  Row-size text, a 14px inline SVG affordance in `--ink-dim`, and a visually-hidden label. The inner
  `<input>` is fully transparent and borderless; the wrapper is the control, and it carries
  `min-width: 0` so it can shrink inside the toolbar.
- **Focus:** `:focus-within` on the wrapper — border turns crimson *and* the amber ring is drawn on
  the wrapper. The ring is the accessible signal; the crimson border is the aesthetic one.
- **Placeholder:** `--ink-dim`, and it shows real example input (`serde, @scope/pkg`), never an
  instruction.

### Navigation

- **Primary nav** is a single 1px `--rule-strong` frame with hairline cell dividers and no gaps — one
  segmented block, not a row of links. Cells are Meta uppercase, `--ink-dim`, hover to `--ink`.
- **Current page reverses to a solid block**: `--ink` background, `--ground` text, weight 700. This is
  the system's one full contrast reversal, and it costs no scanning room.
- **The registry bay** is a ranked list, not a menu: Pixel Small registry name plus a right-aligned
  tabular count, 8/24px rows. The selected row takes `--ink` text, a crimson registry name, a 1px
  crimson left edge and the raised fill — four channels, because no single one of them is
  dependable. Below 900px it becomes a horizontal scroller and the left edge becomes a 2px bottom
  border. Selection **flips attributes in place**; rebuilding the list destroys the element the user
  just activated and drops focus to `<body>`.

### Preferences (settings popover and segmented groups)

The system's two user preferences — **theme** and **locale** — are one pattern, drawn one way.

- **The control is a three-state segmented group**, sharing the primary nav's segment grammar
  exactly: one 1px `--rule-strong` bounding box, 1px cell dividers, `gap: 0`, `flex: 1` cells, Meta
  uppercase at 0.06em, `--ink-dim`, hover to `--ink`, and the chosen cell **reversed to an ink
  block** (`--ink` background, `--ground` text, weight 700) — the same reversal the current nav cell
  uses. State is `aria-pressed`, and each group is a labelled `role="group"`.
- **Three states, because System is a real answer.** `System | Light | Dark` and
  `System | English | Français`. A two-way switch cannot express "follow the browser", so there is no
  two-way switch.
- **The popover** is a 264px-wide box on `--ground-sunk` inside a 1px `--rule-strong` frame, 16px
  padding, anchored to the right edge of its trigger, 8px below it. Escape closes it, focus returns
  to the trigger, and a click outside dismisses it. The trigger carries `aria-expanded` and
  `aria-controls`; opening moves focus to the first cell of the first group.
- **The note under each group** is Meta `--ink-dim` and appears only while the preference is System,
  stating what System currently resolves to ("System follows your device — currently Dark").
- **A preference change announces itself** through the shared `role="status"` live region.

### Cards / Containers

There are none. The system has no card. The only bounded boxes are the **panel** — a 1px
`--rule-strong` frame with 40/24px padding, used for empty, loading and error states, centred, with a
Pixel Small heading and body copy capped at 64ch — and the settings popover above. The panel's error
variant turns the frame and the heading crimson and nothing else. A panel never nests inside another
panel and never carries a shadow.

### Data table

- **Container:** the table always sits in a `.table-wrap` scroll container (see The Own-Container
  Overflow Rule).
- **Head:** Meta uppercase at 0.14em tracking, `--ink-dim`, above a 1px solid `--rule-strong` rule.
  A visible `<caption>`, left-aligned, states the row count and the sort.
- **Rows:** 12px vertical padding, baseline-aligned, divided by 1px dashed `--rule-soft`.
- **Hover:** the raised fill, the divider goes solid `--rule-strong`, and the package name underlines
  in `--rule-strong`. Three channels again; the fill alone would be invisible in either rendition.
- **Columns:** State and Version are width-classed so the eye can track them down the page; the
  package name is the only fluid column, and every fixed width releases below 900px. Numeric columns
  are right-aligned, tabular, and `white-space: nowrap`.

### Resolution as State (signature)

The system's memorable device, and the reason the world exists. It is defined entirely in tokens and
`currentColor`, so it crosses renditions with no override.

- **Fine** — a 3×3 matrix of 5px cells, 1px gutters: BatleHub holds this artifact and has verified it.
- **Coarse** — a 2×2 matrix of 8px cells, 1px gutters: it does not.
- Cells inherit `currentColor`; unlit cells sit at 18% opacity, lit cells at 100%. The matrix is
  `aria-hidden` and always accompanied by its word in Meta uppercase.
- The six states and their three channels:

| State | Matrix | Lit | Colour | Meaning |
|---|---|---|---|---|
| Cached | fine 3×3 | all 9 | `--ink` | held and verified |
| Stale | fine 3×3 | 8 of 9, centre out | `--copper` | held, past its freshness window |
| Held | coarse 2×2 | 3 of 4 | `--copper` | withheld by the release-age gate |
| Pending | coarse 2×2 | 2 of 4 | `--ink-dim` | not yet resolved |
| Yanked | coarse 2×2 | 1 of 4 | `--ink-dim` | withdrawn upstream |
| Blocked | coarse 2×2 | all 4 | `--accent` | refused by policy |

- **Motion.** One authored animation, `resolve`: `filter: blur(2.5px) contrast(1.7); opacity: .55` →
  none, over **520ms `cubic-bezier(.16, 1, .3, 1)`**. It plays on rows that have just arrived or just
  changed state — never on load of unchanged content, never on hover, never on more than the rows
  that changed. `prefers-reduced-motion: reduce` removes it and every transition in the system
  outright, with no crossfade substitute.

### The denial note

Every refusal states its rule in its own row, directly under the artifact it refused, tied to it with
`aria-describedby` — never one banner floating above the table. The note is Body-size `--ink-dim`,
introduced by a 1px full-height `--rule-strong` tie bar, with the rule's name in crimson and a single
inline link to the check that would explain it. A denial that cannot name its rule does not get a
note; it gets a state.

### Named Rules

**The Three Channel Rule.** Every state is carried by pattern, word and hue together. Any one of them
must be sufficient: a reader glancing from a second screen reads the matrix, a colour-blind reader
reads the word, a reader who knows the system reads the hue.

**The One Motion Rule.** The system animates exactly one thing — an artifact resolving. Nothing else
moves: no hover lifts, no page transitions, no skeleton shimmer, no easing on the fill changes.

**The Rule-Beside-Its-Row Rule.** Policy explanations live in the row of the thing they refused.
Never aggregate refusals into a summary banner.

**The Stored-Preference Rule.** For theme and for locale alike, **store the preference
(`system|light|dark`, `system|en|fr`) and never the resolved value.** Storing what it resolved to
silently swallows a later change to the OS theme or the browser language, and the reader who chose
"System" never chose a frozen answer. The resolved value is what renders; the stored one is what is
remembered. Theme resolves **before first paint**, from an inline `<head>` script that writes
`data-theme` on the root element, so there is no flash; both preferences re-resolve when the
environment changes **only while the preference is still System**; and a storage failure (private
mode) degrades to session-only, never to a thrown error.

## Do's and Don'ts

### Do:

- **Do** author every colour token in-gamut for sRGB as written, in every rendition, and keep the
  Monofolio source value as a provenance comment (The In-Gamut Rule — two confirmed cases).
- **Do** put `--accent-ink` on every crimson fill, and pick the pole that clears AA against *that
  rendition's* crimson: dark ink at 5.69:1 on near-black, paper ink at 5.97:1 on paper
  (The Counter-Ink Rule).
- **Do** carry state on rule weight, ink and pattern, and treat `--ground-raised` / `--ground-sunk`
  as confirmation only — 1.06:1 on near-black and 1.11:1 on paper (The Undependable Fill Rule).
- **Do** port a token's hue across renditions and re-derive its lightness against the new ground, then
  re-measure every pair it participates in (The Re-Derived Lightness Rule).
- **Do** write components rendition-blind: ask for `--accent`, never for a rendition. The plate's
  opacity and fill are the only rendition-specific rules the system is allowed.
- **Do** store `system|light|dark` and `system|en|fr` as preferences, resolve theme before first
  paint, and re-resolve on environment change only while the preference is System.
- **Do** draw every mutually-exclusive choice set as one bounding box with 1px dividers and the
  chosen cell reversed to an ink block — the same grammar in the nav and in the settings popover.
- **Do** size Silkscreen at integer multiples of 8px and step it discretely per breakpoint
  (16 / 24 / 56 / 72 / 88 / 104).
- **Do** keep all small text at `--ink-dim` (5.62:1 dark, 7.24:1 light) or better, and never below
  12px.
- **Do** give every state three channels — pattern, word, hue — and label every matrix with its word.
- **Do** put wide content in its own `overflow-x: auto` container and use `overflow-wrap: anywhere`
  on unbreakable identifiers; the body must never scroll horizontally.
- **Do** drop whole table columns when width runs out, and release the remaining fixed widths to
  `auto`; State, Package and Version stay at every size.
- **Do** confine texture to boxes that hold no text and cannot acquire any, and declare a masked
  element's fill only inside the `@supports` query that confirms the mask.
- **Do** keep the amber focus ring on everything focusable, at `2px` with `2px` offset.
- **Do** state each refusal's rule in its own row, tied by `aria-describedby`.
- **Do** move focus deliberately after any re-render that destroys the focused element — flip
  attributes in place where possible, and otherwise land focus on the surface the action produced.

### Don't:

- **Don't** treat the light rendition as a filter over the dark one, or the dark one as the "real"
  design. Paper is the world's home ground; near-black is the adaptation the use scene earned. Both
  are measured, and neither gets its own component rules.
- **Don't** introduce a card, a tile, or a stat row. The world refuses the card grid and the
  stat-tile row by name; regions are separated by full-width hairlines.
- **Don't** add a corner radius. Every corner is square, including ported Monofolio components that
  arrive with 2px.
- **Don't** add a shadow, a blur, or a glow — not even under the settings popover. The only two
  box-shadows are the zero-blur ring and pixel step on the primary action; `--cyber-glow` and
  `--steam-glow` do not exist in this world.
- **Don't** paint a thick coloured side stripe to mark selection — 1px lit edge plus ink does it.
- **Don't** use `clamp()` or any fluid sizing on the bitmap face.
- **Don't** let hue alone carry a state, and don't add a hue to signal a new condition — the dot
  pattern carries it.
- **Don't** spend crimson outside its four jobs (link text, the one filled action, `blocked`,
  selection edge), and don't let it appear in a disabled control.
- **Don't** use copper to mean "healthy" — it means waiting or held.
- **Don't** store the resolved theme or locale, and don't offer a two-way theme switch — "System" is
  a real answer that a boolean cannot express.
- **Don't** use `word-break: break-word` for overflow; it is deprecated and several engines treat it
  as `normal` in exactly the case that matters.
- **Don't** set a fixed column width that cannot release below the collapse breakpoint.
- **Don't** ship a font as "the closest installed face"; both faces are self-hosted, preloaded, and
  set to `swap`.
- **Don't** use an icon font or a glyph-as-icon; icons are inline SVG at 14–16px, 1.5px stroke.
- **Don't** animate anything other than an artifact resolving, and don't substitute a crossfade when
  `prefers-reduced-motion` is set — remove the motion.
- **Don't** re-introduce the decorative two-axis grid background. Every line separates two real
  things.
