<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { RouterLink } from "vue-router";
import { useAuth } from "@/composables/useAuth";
import { formatBytes, formatRelative } from "@/lib/format";
import {
  STATE_LABEL_KEYS,
  detailPath,
  matchLabel,
  noteKey,
  rowId,
  rowSnippet,
  rowState,
  type ExploreRow,
  type UpstreamRow,
} from "@/lib/catalogRow";
import { Resolution } from "@/components/ui/resolution";
import { Button } from "@/components/ui/button";
import { RuleNote } from "@/components/ui/rule-note";
import { TableRow, TableCell } from "@/components/ui/table";

/** One result of the catalog: its row, and the snippet, fetch and note rows
    that follow it when it has them. */
defineProps<{
  row: ExploreRow;
  /** The facet's selection; a row names its registry only when it is "all". */
  selectedRegistry: string | null;
  /** Which row is being fetched, and what came of the last attempt. */
  fetching: string | null;
  fetchResult: Record<string, string>;
}>();

const emit = defineEmits<{
  fetch: [row: UpstreamRow];
}>();

const { t } = useI18n();

const { isAuthenticated } = useAuth();

/**
 * Whether this row offers a fetch: upstream-only, and the server said yes.
 *
 * `fetch?.` although the generated type says the field is always there. It is
 * always there in a *fresh* response; the upstream half of this page is served
 * from a ten-minute in-memory cache, so a reader whose tab outlived a deploy
 * has rows from the server that had no such field. Optional chaining draws no
 * button for them, which is the right answer, where a hard read throws inside
 * the render and takes the whole listing down — including its cached rows,
 * which had nothing to do with it.
 */
function canFetch(row: ExploreRow): boolean {
  return row.kind === "upstream" && row.fetch?.offered === true && isAuthenticated.value;
}
</script>

<template>
  <!-- No `@click` and no `cursor-pointer`: the row is not the
       control, the name is. A whole-row handler is unreachable by
       keyboard and invisible to assistive tech, and dressing it as
       clickable promised a target that only a mouse could take. -->
  <TableRow :class="['align-baseline', noteKey(row) ? 'border-b-0' : '']">
    <TableCell class="pl-0 pr-3 py-3 align-baseline">
      <Resolution :state="rowState(row)" :label="t(STATE_LABEL_KEYS[rowState(row)])" />
    </TableCell>
    <!-- `aria-describedby` is what ties a denial to the package it is
       about. Without it the note below is a paragraph a screen
       reader meets after the row and has to relate by proximity.

       `overflow-wrap:anywhere` is what the proof sets on `.pkg` —
       not `break-words`. The two differ in exactly the way that
       matters here: `anywhere` is taken into account when the
       browser computes the cell's *min-content* width, `break-word`
       is not. So with `break-words` a name like
       `github.com/spf13/cobra` still demanded its full width, the
       table grew past the viewport, and the whole catalog sat
       behind a horizontal scrollbar at 390px. -->
    <TableCell
      class="[overflow-wrap:anywhere] pl-0 pr-3 py-3 align-baseline text-base"
      :aria-describedby="noteKey(row) ? `note-${rowId(row)}` : undefined"
    >
      <!-- The package name *is* the link — the pattern the version
           list on the detail page already uses. `anywhere` rather
           than `break-words` for the same min-content reason as the
           cell above, and an underline on hover because the row no
           longer carries any other affordance. -->
      <RouterLink
        :to="detailPath(row)"
        class="[overflow-wrap:anywhere] hover:underline underline-offset-[3px]"
      >
        {{ row.name }}
      </RouterLink>
      <!-- One list, provenance stated per row. The question a reader
         actually has is "does this instance already have it?", and
         splitting the table into two makes that harder to answer,
         not easier. -->
      <span
        v-if="row.kind === 'upstream'"
        class="ml-2 text-xs uppercase tracking-[0.1em] text-muted-foreground"
        >{{ t("packageCatalog.upstreamRow") }}</span
      >
      <span
        v-else-if="!selectedRegistry"
        class="ml-2 text-xs uppercase tracking-[0.1em] text-muted-foreground"
        >{{ row.registry }}</span
      >
      <!-- Why this row is here. Shown only for a prose match: a row
         that matched on its name needs no explanation, and labelling
         every row would make the one that needs it invisible. -->
      <span
        v-if="matchLabel(t, row)"
        class="ml-2 text-xs uppercase tracking-[0.1em] text-muted-foreground"
        :title="t('packageCatalog.matchedInLabel', { where: matchLabel(t, row) })"
        >{{ matchLabel(t, row) }}</span
      >
    </TableCell>
    <TableCell class="pl-0 pr-3 py-3 align-baseline text-muted-foreground tabular-nums">
      <template v-if="row.kind === 'cached'">{{ row.newest_version ?? "—" }}</template>
      <template v-else>{{ row.latest_version ?? "—" }}</template>
    </TableCell>
    <TableCell
      class="hidden pl-0 pr-3 py-3 text-right align-baseline text-muted-foreground tabular-nums lg:table-cell"
    >
      <template v-if="row.kind === 'cached'">{{ formatBytes(row.cached_bytes) }}</template>
      <template v-else>—</template>
    </TableCell>
    <TableCell
      class="hidden whitespace-nowrap px-0 py-3 text-right align-baseline text-muted-foreground tabular-nums lg:table-cell"
    >
      <template v-if="row.kind === 'cached'">{{
        formatRelative(row.last_fetched_at, { fallback: "—" })
      }}</template>
      <template v-else>—</template>
    </TableCell>
  </TableRow>

  <!-- Every denial states its rule in its own row, tied to the
     package by `aria-describedby` — not one note floating above the
     table. The rule is indented under the package rather than under
     the state, so the eye reads it as belonging to the name. -->
  <!-- The matched fragment, as **text**. It is interpolated, never
     `v-html`: the README panel's markup boundary is a deliberate,
     tested, single-component one, and a search snippet is a second
     surface for package-authored content reached by a much cheaper
     path — no navigation, just a query (RFC 0007-bis §7.4). -->
  <TableRow v-if="rowSnippet(row)" class="border-dashed border-rule-soft hover:bg-transparent">
    <TableCell class="pl-0 pr-3 pb-3 pt-0" />
    <TableCell colspan="4" class="pl-0 pr-3 pb-3 pt-0">
      <RuleNote>
        {{ rowSnippet(row) }}
      </RuleNote>
    </TableCell>
  </TableRow>

  <!-- The action for an upstream row, in a row of its own rather
     than as a chip in a cell — the shape the snippet and the
     refusal note already use here, and the one the package page
     arrived at after its own fetch button spent a while as a 24px
     chip in a table column.

     The label names the version, which is the version the upstream
     search returned and the one the cell above shows. Nothing is
     being chosen on this screen (RFC 0007-bis §11 q3).

     On success no message is written: the listing and the upstream
     results are both refetched, the row leaves the upstream half
     and reappears as a held one, and its state chip is a better
     answer than a sentence under a row that no longer exists. -->
  <TableRow
    v-if="canFetch(row) || fetchResult[rowId(row)]"
    class="border-dashed border-rule-soft hover:bg-transparent"
  >
    <TableCell class="pl-0 pr-3 pb-3 pt-0" />
    <!-- `colspan="2"` plus the two trailing cells the wide columns
       use, exactly as the note row below does. A single
       `colspan="4"` declares five columns on a row whose table
       renders three below `lg` — the last two `<TableHead>`s are
       `hidden lg:table-cell` — and the browser then widens the
       whole table to five, shifting the header away from the body
       on every viewport under the breakpoint. -->
    <TableCell colspan="2" class="pl-0 pr-3 pb-3 pt-0">
      <div class="flex flex-wrap items-center gap-3">
        <!-- The visible label names the version, which is all a
           reader who can see the row it sits under needs. The
           accessible name adds the package: a screen-reader user
           arrives at this button out of that context, and "Fetch
           9.9.9" on a page of fifty hits names nothing. -->
        <Button
          v-if="canFetch(row)"
          variant="outline"
          size="sm"
          :disabled="fetching !== null"
          :aria-label="
            t('packageCatalog.fetchVersionLabel', {
              name: row.name,
              version: (row as UpstreamRow).latest_version,
            })
          "
          @click="emit('fetch', row as UpstreamRow)"
        >
          {{
            fetching === rowId(row)
              ? t("packageCatalog.fetching")
              : t("packageCatalog.fetchVersion", {
                  version: (row as UpstreamRow).latest_version,
                })
          }}
        </Button>
        <output v-if="fetchResult[rowId(row)]" class="text-sm text-muted-foreground">
          {{ fetchResult[rowId(row)] }}
        </output>
      </div>
    </TableCell>
    <TableCell class="hidden px-0 pb-3 pt-0 lg:table-cell" />
    <TableCell class="hidden px-0 pb-3 pt-0 lg:table-cell" />
  </TableRow>

  <TableRow v-if="noteKey(row)" class="border-dashed border-rule-soft hover:bg-transparent">
    <TableCell class="pl-0 pr-3 pb-3 pt-0" />
    <TableCell colspan="2" class="pl-0 pr-3 pb-3 pt-0">
      <RuleNote :id="`note-${rowId(row)}`">
        {{ t(noteKey(row)!) }}
        <!-- Underlined at rest, not on hover. Crimson on the
               note's dim ink measures 1.28:1 against the surrounding
               text, so colour alone cannot mark it as a link — axe's
               `link-in-text-block` wants 3:1 or a non-colour cue,
               and the design gate failed on exactly that. The proof
               underlines it too: its links take the browser default
               and it only sets the offset. -->
        <RouterLink
          v-if="row.kind === 'cached'"
          class="text-primary underline underline-offset-[3px]"
          :to="detailPath(row)"
          >{{ t("packageCatalog.noteDetails") }}</RouterLink
        >
      </RuleNote>
    </TableCell>
    <TableCell class="hidden px-0 pb-3 pt-0 lg:table-cell" />
    <TableCell class="hidden px-0 pb-3 pt-0 lg:table-cell" />
  </TableRow>
</template>
