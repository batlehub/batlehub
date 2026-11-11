<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { RouterLink, useRoute } from "vue-router";
import type { ExploreVersionDto, FirewallDto, UpstreamReadDto } from "@/client/types.gen";
import { isUpstreamOnly, rowState, STATE_LABEL_KEYS } from "@/composables/usePackageVersions";
import { formatCount, formatDay } from "@/lib/format";
import UpstreamNotice from "@/components/package/UpstreamNotice.vue";
import { Badge } from "@/components/ui/badge";
import { Resolution } from "@/components/ui/resolution";
import { EmptyState } from "@/components/ui/empty-state";
import { Button } from "@/components/ui/button";
import { SearchInput } from "@/components/ui/search-input";
import { Pagination } from "@/components/ui/pagination";
import {
  Table,
  TableHeader,
  TableHead,
  TableBody,
  TableRow,
  TableCell,
} from "@/components/ui/table";

/** The version index: the pre-release toggle, the filter, one page of rows and
    the pager. Every control is an emit; the state is `usePackageVersions`'. */
const props = defineProps<{
  name: string;
  upstream: UpstreamReadDto;
  pagedVersions: ExploreVersionDto[];
  selectedVersion: string | null;
  showPrereleases: boolean;
  prereleaseCount: number;
  hiddenPrereleaseCount: number;
  upstreamVersionCount: number;
  versionFilter: string;
  filterPending: boolean;
  filteredTotal: number;
  unfilteredTotal: number;
  versionPage: number;
  versionTotalPages: number;
  refShas: Record<string, string>;
}>();

const emit = defineEmits<{
  "toggle-prereleases": [];
  filter: [value: string];
  "turn-page": [page: number];
}>();

const { t } = useI18n();
const route = useRoute();

const stateLabel = (row: { state?: string; firewall: FirewallDto }) =>
  t(STATE_LABEL_KEYS[rowState(row)]);

/** Where the row's version link points: this page, with that version selected. */
function versionLink(version: string) {
  return { path: route.path, query: { ...route.query, version } };
}

/** git's own abbreviation. */
const shortSha = (version: string) => props.refShas[version]?.slice(0, 7) ?? null;
</script>

<template>
  <!-- ── The index ─────────────────────────────────────────────────────
       What the table is now: the list you pick the subject from, not the
       page. Everything a reader compares *between* versions stays here;
       everything they read *about* one version moved up to the subject —
       which is what let this go from nine columns to six, three of which
       survive at 390px where five of the nine used to be behind a
       horizontal scroll with no affordance. -->
  <section class="border-t border-rule-soft pt-6" aria-labelledby="versions-heading">
    <div class="space-y-3">
      <div class="flex flex-wrap items-center justify-between gap-3">
        <h2 id="versions-heading" class="font-mono text-base text-foreground">
          {{ t("common.versions") }}
        </h2>
        <!-- The filter says what it is hiding. A control reading "Show
             pre-releases" over a list that has none is a promise of
             something to reveal, so it renders only when there is; and the
             count is in the label rather than in a tooltip, because "8
             hidden" is the fact that decides whether a reader clicks. -->
        <!-- No `aria-pressed`. The label already inverts — "Hide 8
             pre-releases" — so a pressed state on top of it announces as
             "Hide 8 pre-releases, pressed", which is a double negative for
             the one reader who cannot see the list change. One channel. -->
        <Button
          v-if="prereleaseCount > 0"
          variant="outline"
          size="sm"
          class="px-2 text-xs"
          @click="emit('toggle-prereleases')"
        >
          {{
            showPrereleases
              ? t("packageDetailPage.hidePrereleases", prereleaseCount)
              : t("packageDetailPage.showPrereleases", hiddenPrereleaseCount)
          }}
        </Button>
      </div>
      <div v-if="upstream.attempted">
        <UpstreamNotice :upstream="upstream" :upstream-version-count="upstreamVersionCount" />
      </div>
      <!-- The filter sits with the list it filters rather than in the card
           header with the pre-release control: that one changes *what the
           list is*, this one searches inside it, and putting them together
           would read as two halves of one filter. -->
      <div v-if="unfilteredTotal > 1" class="flex flex-wrap items-center gap-3">
        <SearchInput
          :placeholder="t('packageDetailPage.filterVersions')"
          :aria-label="t('packageDetailPage.filterVersions')"
          :value="versionFilter"
          @input="emit('filter', ($event.target as HTMLInputElement).value)"
        />
        <!-- The live region that answers "did my filter do anything".
             It is not the only one on the page — `Pagination` and
             `UpstreamNotice` are shared components and carry their own, and
             taking those away here would change every page that uses them —
             so this comment states the position rather than a fix that was
             never made: three announcements can still land on one page turn,
             and consolidating them is a change to those components. -->
        <output class="text-xs text-muted-foreground" aria-live="polite">
          {{
            filterPending
              ? t("packageDetailPage.filtering")
              : t("packageDetailPage.versionsShown", {
                  shown: filteredTotal,
                  total: unfilteredTotal,
                })
          }}
        </output>
      </div>
      <Table :label="t('packageDetailPage.tableLabel', { name })">
        <!-- A visible caption stating the count and the order.
             The list had neither, so a reader on page 4 of 160 could not
             know whether they were walking forward or backward through
             time — and `Table` was called three times on an admin's view
             with no `label`, which announced all three scroll regions with
             the same generic name. -->
        <caption class="caption-top pb-3 text-left text-xs text-muted-foreground">
          {{
            t("packageDetailPage.tableCaption", {
              shown: filteredTotal,
              total: unfilteredTotal,
            })
          }}
        </caption>
        <TableHeader>
          <!-- One solid rule under the head, dashed between the rows: the
               proof's grammar, ported from the catalog rather than restated.
               Cells are flush left and padded right, so the first column
               lines up with the caption and the specimen above it. -->
          <TableRow class="border-b border-solid border-border hover:bg-transparent">
            <TableHead class="pl-0 pr-3 lg:w-[9.5rem]">{{ t("common.state") }}</TableHead>
            <TableHead class="pl-0 pr-3">{{ t("common.version") }}</TableHead>
            <TableHead class="pl-0 pr-3 lg:w-[8rem]">{{ t("common.source") }}</TableHead>
            <!-- Dropped below `lg`, whole, rather than squeezed: at 390px
                 the nine columns this table used to draw claimed 895px of a
                 356px box, so five of them sat behind a scroll with no
                 affordance. Every fact they carry is on the subject above
                 for the version a reader has selected — which is what makes
                 dropping them honest rather than hiding them. -->
            <TableHead class="hidden pl-0 pr-3 text-right lg:table-cell lg:w-28">{{
              t("common.downloads")
            }}</TableHead>
            <TableHead class="hidden pl-0 pr-3 text-right lg:table-cell lg:w-32">{{
              t("common.published")
            }}</TableHead>
            <TableHead class="hidden px-0 lg:table-cell lg:w-32">{{
              t("common.security")
            }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <!-- Selection rides on ink and a lit edge, never on fill.

               It was `bg-muted/40`, and DESIGN.md's Undependable Fill Rule
               is about exactly this: on this ground the raised fill computes
               to about 1.06:1 against the sheet, so the marked row was
               indistinguishable from every other one — the page chose a
               default version and then had no way to say which. `Facet`
               already solved this for the registry bay ("selection rides on
               ink and a lit edge, not a fill"), and this is the same
               statement in a table: a crimson left edge, the ink lifted to
               `--ink` and the weight raised.

               The transparent edge on unselected rows reserves the same 2px,
               so marking a row moves nothing sideways.

               `aria-current="true"` carries it to a screen reader, which
               cannot see either the rule or the weight. -->
          <TableRow
            v-for="ver in pagedVersions"
            :key="`${ver.version}-${ver.source}`"
            :class="[
              'border-l border-rule-soft align-baseline hover:border-solid hover:border-border',
              /* The dashed/solid pair is per-row, not per-side: `border-l-solid`
                 is not a Tailwind utility and is defined nowhere in this repo,
                 so writing it left the whole row dashed — including the crimson
                 selection edge, which is the one edge the world says must read
                 as engaged. A selected row is solid on every side; the rest stay
                 dashed at rest and go solid under the pointer. */
              selectedVersion === ver.version
                ? 'border-solid border-l-primary font-semibold text-foreground'
                : 'border-dashed border-l-transparent',
              ver.is_prerelease && selectedVersion !== ver.version
                ? 'text-muted-foreground italic'
                : '',
              ver.is_prerelease && selectedVersion === ver.version ? 'italic' : '',
            ]"
            :aria-current="selectedVersion === ver.version ? 'true' : undefined"
          >
            <TableCell class="pl-0 pr-3 py-3 align-baseline">
              <Resolution :state="rowState(ver)" :label="stateLabel(ver)" />
            </TableCell>
            <TableCell class="pl-0 pr-3 py-3 align-baseline font-mono text-sm">
              <!-- A link, not a click handler on the `<tr>`.
                   The row was `@click` with `cursor-pointer`, no `tabindex`,
                   no `role` and no key handler, so selecting a version — the
                   page's central interaction, which drives both the subject
                   above and the URL — was unavailable to a keyboard while
                   `aria-current` announced a selection that reader could not
                   change. A link brings the keyboard, the focus ring,
                   middle-click-to-a-new-tab and a copyable target with it,
                   and it takes the six nested buttons out of a clickable
                   row.

                   Selection is carried by the crimson 1px edge on the row,
                   the ink, the weight and `aria-current` — four channels,
                   because the fill this used to use computes to 1.06:1 and
                   the 2px coloured stripe it used to draw is a thing the
                   world refuses by name. -->
              <!-- `replace`, never `push` — the rule stated at the top of
                   this file and not applied here when the click handler
                   became a link. Reading down a version list is not ten
                   destinations: without it, five clicks were five Back
                   presses, and worse, `history.state.back` stopped being
                   `/packages`, so `goBack()` fell through to its
                   reconstruct-by-hand path and dropped the reader on a
                   catalog with its search, scope, sort, page and scroll
                   offset gone — the exact loss the comment at `goBack` says
                   it prevents. One attribute restores both. -->
              <RouterLink
                replace
                :to="versionLink(ver.version)"
                class="inline-block py-1 [overflow-wrap:anywhere] hover:underline underline-offset-[3px]"
                :class="selectedVersion === ver.version ? 'text-primary' : ''"
              >
                {{ ver.version }}
              </RouterLink>
              <!-- RFC 0019 §6.5: the commit this ref resolved to. -->
              <span
                v-if="shortSha(ver.version)"
                class="ml-2 text-xs text-muted-foreground"
                :title="refShas[ver.version]"
                data-testid="ref-sha"
                >{{ shortSha(ver.version) }}</span
              >
              <Badge v-if="ver.is_prerelease" variant="outline" class="ml-2 text-xs">
                {{ t("packageDetailPage.prerelease") }}
              </Badge>
              <!-- Copper, not crimson: the subject region already sets this
                   fact in copper, and crimson is spoken for. One page, one
                   fact, one hue. -->
              <Badge v-if="ver.deprecated" variant="outline" class="ml-2 text-xs text-copper">
                {{ t("packageDetailPage.deprecated") }}
              </Badge>
              <Badge v-if="ver.unlisted" variant="secondary" class="ml-2 text-xs">
                {{ t("packageDetailPage.unlisted") }}
              </Badge>
              <!-- Under the version rather than in a column of its own: the
                   licence is an attribute of this version, and it is the
                   kind of fact a reader compares down a column at a glance.

                   A stated "unknown" rather than a blank when null —
                   rendering nothing would make "no manifest parser for this
                   registry type" indistinguishable from "declares no
                   licence" (RFC 0004-bis §13.1). -->
              <p class="mt-1 text-xs text-muted-foreground [overflow-wrap:anywhere]">
                {{ ver.license ?? t("packageDetailPage.licenseUnknown") }}
              </p>
            </TableCell>
            <TableCell class="pl-0 pr-3 py-3 align-baseline">
              <!-- Three values, not two. `upstream` means this instance
                   holds no bytes for the version and knows about it only
                   because it asked — the badge says so rather than letting
                   it read as something we have.

                   The fetch button that used to live beside it moved to the
                   subject above: it is the one act on this page that spends
                   bandwidth and writes an audit row, and it was a 24px
                   outline chip repeated twenty-five times down a column. -->
              <Badge
                :variant="ver.source === 'local' ? 'secondary' : 'outline'"
                class="text-xs"
                :class="isUpstreamOnly(ver.source) ? 'border-dashed' : ''"
              >
                {{
                  ver.source === "local"
                    ? t("packageDetailPage.local")
                    : isUpstreamOnly(ver.source)
                      ? t("packageDetailPage.notHeldHere")
                      : t("common.proxied")
                }}
              </Badge>
            </TableCell>
            <!-- `unknown`, never `0`: a definite-looking number for a
                 version this instance has never held would be a claim we
                 cannot support (RFC 0007 §4.2). -->
            <TableCell
              class="hidden pl-0 pr-3 py-3 text-right align-baseline text-muted-foreground tabular-nums lg:table-cell"
            >
              {{
                ver.download_count === null || ver.download_count === undefined
                  ? t("common.unknown")
                  : formatCount(ver.download_count)
              }}
            </TableCell>
            <TableCell
              class="hidden whitespace-nowrap pl-0 pr-3 py-3 text-right align-baseline text-muted-foreground tabular-nums lg:table-cell"
            >
              {{ formatDay(ver.published_at ?? null) }}
            </TableCell>
            <TableCell class="hidden px-0 py-3 align-baseline text-muted-foreground lg:table-cell">
              <!-- A count, not five hoverable chips. What each advisory
                   *says* is on the subject above, in text, for the version
                   the reader selected — which is the only place it was ever
                   readable without a mouse. -->
              <span v-if="!ver.vulnerabilities_scanned" class="text-xs">
                {{ t("packageDetailPage.notScanned") }}
              </span>
              <span v-else-if="ver.vulnerabilities.length" class="text-xs text-destructive">
                {{ t("packageDetailPage.vulnerabilityCount", ver.vulnerabilities.length) }}
              </span>
              <span v-else class="text-xs">{{ t("packageDetailPage.noKnownVulns") }}</span>
            </TableCell>
          </TableRow>
          <!-- A filter that matched nothing is not a package with no
               versions, and answering the first with the second's words
               ("nothing has been pulled through") would describe the
               instance to a reader who asked about their own typing. The
               counter above already says `0 of 60 shown`; this says which
               of the two absences the empty table is. -->
          <TableRow v-if="pagedVersions.length === 0 && unfilteredTotal > 0">
            <TableCell colspan="6" class="px-0 py-6 text-center text-muted-foreground">
              <!-- `filtered`, which its own doc calls required rather than
                   inferred, and which this call site omitted — so
                   `data-filtered="false"` was being written on precisely the
                   state the flag exists to mark. -->
              <EmptyState
                filtered
                :title="t('packageDetailPage.noVersionsMatch')"
                :description="t('packageDetailPage.noVersionsMatchHint')"
              />
            </TableCell>
          </TableRow>
          <TableRow v-else-if="pagedVersions.length === 0">
            <TableCell colspan="6" class="px-0 py-6 text-center text-muted-foreground">
              <!-- Two different absences, and the reader needs to know
                   which: "nothing has been pulled through, and the upstream
                   does not have it either" is an answer; "the upstream
                   could not be reached" is a gap. -->
              <EmptyState
                :title="t('packageDetailPage.noVersionsYet')"
                :description="
                  upstream.error
                    ? t('packageDetailPage.upstreamUnreachable')
                    : upstream.attempted
                      ? t('packageDetailPage.upstreamHasNoneEither')
                      : t('packageDetailPage.nothingHasBeenPulledThrough')
                "
              />
            </TableCell>
          </TableRow>
        </TableBody>
      </Table>
      <div v-if="versionTotalPages > 1" class="border-t border-rule-soft pt-3">
        <Pagination
          :page="versionPage"
          :total-pages="versionTotalPages"
          @update:page="emit('turn-page', $event)"
        />
      </div>
    </div>
  </section>
</template>
