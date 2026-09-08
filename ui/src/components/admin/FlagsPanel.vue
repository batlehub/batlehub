<script setup lang="ts">
import { ref, onMounted } from "vue";
import { useI18n } from "vue-i18n";

import { listFlags } from "@/client/sdk.gen";
import type { PackageFlag } from "@/client/types.gen";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardHeader, CardTitle, CardContent } from "@/components/ui/card";
import { Switch } from "@/components/ui/switch";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";
import { formatDay } from "@/lib/format";

/**
 * What every `[[flag_sources]]` entry has said (RFC 0002 §4.5): which source
 * flagged which version, how hard, and whether the flag still stands. Read
 * with `flags:read`; a `403` shows the panel empty with the reason rather
 * than hiding it, so an operator sees the surface exists.
 */
const { t } = useI18n();

const PER_PAGE = 25;
const items = ref<PackageFlag[]>([]);
const total = ref(0);
const page = ref(0);
const includeDead = ref(false);
const loading = ref(false);
const error = ref<string | null>(null);

async function load() {
  loading.value = true;
  error.value = null;
  try {
    const {
      data,
      error: err,
      response,
    } = await listFlags({
      query: { include_dead: includeDead.value, page: page.value, per_page: PER_PAGE },
    });
    if (err || !data) {
      error.value =
        response?.status === 403 ? t("flagsPanel.forbidden") : t("flagsPanel.loadFailed");
      items.value = [];
      return;
    }
    items.value = data.items;
    total.value = data.total;
  } catch {
    error.value = t("flagsPanel.loadFailed");
  } finally {
    loading.value = false;
  }
}

function toggleDead(v: boolean) {
  includeDead.value = v;
  page.value = 0;
  load();
}

function state(f: PackageFlag): string {
  if (f.revoked_at) return t("flagsPanel.revoked");
  if (f.expires_at) return t("flagsPanel.expires", { when: formatDay(f.expires_at) });
  return t("flagsPanel.live");
}

/**
 * The flag URL, if it is a page we are willing to link to.
 *
 * The server normalises this through the same http(s) allow-list every other
 * externally supplied link goes through, so this is the second of two checks,
 * not the only one — but a flag arrives from an integration rather than from
 * us, an `:href` is a navigation sink, and the same guard already sits in
 * `RichText`. Matching the scheme rather than blocking `javascript:` by name
 * leaves nothing to spell around.
 */
function pageUrl(url: string | null | undefined): string | null {
  if (!url) return null;
  const href = url.trim();
  return /^https?:\/\/[^\s/\\]/i.test(href) ? href : null;
}

function effectVariant(effect: string) {
  switch (effect) {
    case "hard_block":
      return "destructive";
    case "gate":
      return "copper";
    default:
      return "secondary";
  }
}

onMounted(load);
</script>

<template>
  <Card data-testid="flags-panel">
    <CardHeader>
      <CardTitle>
        {{ t("flagsPanel.title") }}
        <span v-if="total" class="font-mono text-base font-normal text-muted-foreground"
          >({{ total }})</span
        >
      </CardTitle>
    </CardHeader>
    <CardContent class="space-y-4">
      <p class="text-sm text-muted-foreground">{{ t("flagsPanel.intro") }}</p>

      <div class="flex items-center gap-2">
        <Switch
          id="flags-include-dead"
          :model-value="includeDead"
          data-testid="flags-include-dead"
          @update:model-value="toggleDead"
        />
        <label for="flags-include-dead" class="text-sm">{{ t("flagsPanel.includeDead") }}</label>
      </div>

      <p v-if="error" class="text-sm text-destructive" data-testid="flags-error">{{ error }}</p>

      <Table v-if="items.length" data-testid="flags-rows">
        <TableHeader>
          <TableRow>
            <TableHead>{{ t("flagsPanel.source") }}</TableHead>
            <TableHead>{{ t("common.package") }}</TableHead>
            <TableHead>{{ t("flagsPanel.effect") }}</TableHead>
            <TableHead>{{ t("flagsPanel.kind") }}</TableHead>
            <TableHead>{{ t("flagsPanel.state") }}</TableHead>
            <TableHead>{{ t("flagsPanel.summary") }}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRow v-for="f in items" :key="f.id">
            <TableCell class="font-mono text-xs">{{ f.source }}:{{ f.external_id }}</TableCell>
            <TableCell class="font-mono text-sm">
              {{ f.registry }}/{{ f.package_name }}@{{ f.version }}
            </TableCell>
            <TableCell>
              <Badge :variant="effectVariant(f.effect)" class="text-xs">{{ f.effect }}</Badge>
            </TableCell>
            <TableCell class="font-mono text-xs">{{ f.kind }}</TableCell>
            <TableCell class="text-xs">{{ state(f) }}</TableCell>
            <TableCell class="text-sm">
              {{ f.summary }}
              <a
                v-if="pageUrl(f.url)"
                :href="pageUrl(f.url)!"
                class="ml-1 underline text-muted-foreground"
                target="_blank"
                rel="noopener noreferrer"
                >{{ t("flagsPanel.more") }}</a
              >
            </TableCell>
          </TableRow>
        </TableBody>
      </Table>
      <p
        v-else-if="!loading && !error"
        class="text-sm text-muted-foreground"
        data-testid="flags-empty"
      >
        {{ t("flagsPanel.empty") }}
      </p>

      <div v-if="total > PER_PAGE" class="flex items-center gap-2">
        <Button
          size="sm"
          variant="outline"
          :disabled="loading || page === 0"
          @click="
            page -= 1;
            load();
          "
        >
          {{ t("flagsPanel.previous") }}
        </Button>
        <Button
          size="sm"
          variant="outline"
          :disabled="loading || (page + 1) * PER_PAGE >= total"
          @click="
            page += 1;
            load();
          "
        >
          {{ t("flagsPanel.next") }}
        </Button>
      </div>
    </CardContent>
  </Card>
</template>
