<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { RefreshCw } from "@lucide/vue";
import type { ExplorePackageDetailResponse } from "@/client/types.gen";
import { Button } from "@/components/ui/button";

/** The package page's specimen: the name at the Display step, its caption of
    facts and declared links, and the Refresh control. */
defineProps<{
  data: ExplorePackageDetailResponse;
  unfilteredTotal: number;
}>();

const emit = defineEmits<{ refresh: [] }>();

const { t } = useI18n();

/**
 * A URL shortened to the part a reader recognises: `github.com/owner/repo`.
 *
 * The scheme is noise on every one of these — they are all `https` by the time
 * the server has finished with them — and a trailing slash makes two links to
 * the same page look like two pages. The full URL stays in `href`, so hovering
 * and copying give what was actually declared.
 *
 * Falls back to the raw string if `URL` cannot parse it. That should not happen
 * (the server allow-lists these to http(s)) and it is not a security decision
 * either way — this is a *label*, and the `href` is what a click follows.
 */
function linkLabel(url: string): string {
  try {
    const parsed = new URL(url);
    return `${parsed.host}${parsed.pathname}`.replace(/\/$/, "");
  } catch {
    return url;
  }
}
</script>

<template>
  <!-- The specimen, as the catalog builds it: the page announces its
       subject at the Display step, and the facts sit under it as a caption.

       DESIGN.md gives Display ("Silkscreen 700, 56 → 72 per breakpoint,
       line-height 0.92, 0.02em") to *the subject at the top of
       the sheet, one per view*. This h1 was `text-2xl font-mono` — 24px of
       JetBrains Mono — so the one page dedicated to a single package set its
       name in the reading face at a size the doc reserves for the wordmark,
       and the rendered gate reports `h1 not in the display face` at every
       width and role. The catalog spends this step on the registry; here the
       subject is the package, so it spends it on the package.

       **No `uppercase`, and the caption carries the coordinate.** Silkscreen
       is caps-only — `abg` and `ABG` rasterise to identical pixels, checked
       on a canvas rather than assumed from a screenshot — so the transform
       the catalog's specimen applies would be a decision that changes
       nothing here, and, more to the point, *the headline cannot show a
       package's case at all*. That is fine for a registry label (`NPM`) and
       not fine for a coordinate the reader copies into a manifest, where
       `React-Smooth` and `react-smooth` are two different packages on
       Maven, NuGet and Go. So the exact name is repeated in the caption in
       the text face, which is the one place on this page it was otherwise
       unavailable in its true form.

       `break-words`, **not** `[overflow-wrap:anywhere]` — the opposite of
       the choice the catalog's name *cell* documents, and the difference is
       the size. `anywhere` opens a break opportunity at every character, so
       at 390px `react-smooth` came out as `REACT` / `-` / `SMOOT` / `H`: the
       hyphen alone on one line and an orphan glyph on the next.
       `break-words` breaks as a last resort only, so the browser takes the
       hyphen first (`REACT-` / `SMOOTH`) and splits mid-glyph just when a
       segment cannot fit at all — which at 104px a name like
       `@babel/plugin-transform-runtime` does need. In a 15px table cell the
       min-content sizing is what matters and `anywhere` wins; in a 104px
       headline the break *quality* is what matters. `min-w-0` on the flex
       child is what lets either act.

       The icon and the registry `Badge` came out of the heading line. A
       24px lucide glyph and a bordered chip beside a 104px headline read as
       three competing objects, and the registry is a *fact about* the
       subject, not part of its name — so it joins the caption, in the
       order the catalog's own caption uses. -->
  <!-- A column until `sm`, a row above it. The button was a sibling of the
       specimen in one wrapping flex row, and `flex-1` + `min-w-0` means the
       *text* yields, not the button: at 390 the h1 got 255px of a 358px
       line, so a 56px headline broke as `REACT` / `-` / `SMOOT` / `H`
       inside a column two glyphs narrower than the page. Stacking gives the
       subject the whole line — `REACT-` / `SMOOTH` — and costs a control
       nothing, since Refresh is the same size wherever it sits. -->
  <div class="flex flex-col gap-3 sm:flex-row sm:items-start">
    <div class="min-w-0 flex-1">
      <h1 class="font-display font-bold tracking-[0.02em] leading-[0.92] text-display break-words">
        {{ data.name }}
      </h1>
      <!-- The separator is `text-muted-foreground`, not `text-border`.
           `--rule-soft` is declared for separators and is explicitly *not
           contrast-carrying*; painting a glyph in it put a 13px character at
           3.40:1 in the light rendition and 3.67:1 in the dark, measured.
           It is `aria-hidden`, so it is decoration to a screen reader and
           still text to an eye. -->
      <p class="mt-3 text-sm text-muted-foreground [overflow-wrap:anywhere]">
        <span class="font-mono text-foreground">{{ data.name }}</span>
        <span class="px-2" aria-hidden="true">·</span>
        <span class="text-foreground">{{ data.registry }}</span>
        <span class="px-2" aria-hidden="true">·</span>
        {{ t("packageDetailPage.knownVersions", unfilteredTotal) }}
        <!-- Membership is stated only when it is true, and it is stated as
             what it *does*. This was an "Access Gate" card whose one
             surviving row read "Beta channel — Non-member" on every package
             of every registry, including the ones with no beta channel
             configured, where the words describe nothing. A member sees
             versions others do not, which is a fact about what is on this
             page; a non-member has nothing to be told. -->
        <template v-if="data.gate.beta_member">
          <span class="px-2" aria-hidden="true">·</span>
          <span class="text-copper">{{ t("packageDetailPage.betaMemberFact") }}</span>
        </template>
      </p>
      <!-- The package's own links, when its metadata declared any. Absent
           rather than disabled when it did not: a greyed-out "Source code"
           on a package that never named a repository states a fact we do not
           have. The server has already normalised and allow-listed these to
           http(s) — see `PackageLinksDto`. -->
      <p
        v-if="data.links?.repository || data.links?.homepage"
        class="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1 text-sm"
      >
        <a
          v-if="data.links?.repository"
          :href="data.links.repository"
          target="_blank"
          rel="noopener noreferrer"
          class="inline-flex items-center gap-1 py-1 underline underline-offset-2 [overflow-wrap:anywhere]"
        >
          {{ t("packageDetailPage.sourceCode") }}
          <span class="text-muted-foreground">{{ linkLabel(data.links.repository) }}</span>
        </a>
        <a
          v-if="data.links?.homepage"
          :href="data.links.homepage"
          target="_blank"
          rel="noopener noreferrer"
          class="inline-flex items-center gap-1 py-1 underline underline-offset-2 [overflow-wrap:anywhere]"
        >
          {{ t("packageDetailPage.homepage") }}
          <span class="text-muted-foreground">{{ linkLabel(data.links.homepage) }}</span>
        </a>
      </p>
    </div>
    <Button variant="outline" size="sm" @click="emit('refresh')">
      <RefreshCw class="mr-1 h-4 w-4" aria-hidden="true" />
      {{ t("common.refresh") }}
    </Button>
  </div>
</template>
