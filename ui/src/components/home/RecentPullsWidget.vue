<script setup lang="ts">
/**
 * "What have I actually pulled?" — the third `me` read path (RFC 0004 §1).
 *
 * `GET /api/v1/me/downloads` shipped in Phase 2 with its scoping, its absence
 * tests and its Postgres query, and then had no reader at all: §4.2 specified
 * two widgets and the endpoint was built to §1's list. A read path with no
 * consumer is the same defect as §2.4's data with no read path, pointed the
 * other way.
 *
 * It is a different question from the advisories widget beside it. That one
 * answers "is any of this known-bad" and is silent when everything is fine —
 * which is most of the time, and which tells a developer nothing about what
 * their machine is actually resolving through this instance. This one answers
 * "what did I pull", including the ordinary case, and it is the only surface
 * where a developer can see their own traffic without an admin.
 *
 * Only this caller's rows, only successful downloads — the filter is in
 * `PackageRepository::list_own_downloads`, not assembled here.
 */
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { RouterLink } from "vue-router";
import { myDownloads } from "@/client/sdk.gen";
import type { MyDownloadDto } from "@/client/types.gen";
import { useApi } from "@/composables/useApi";
import { useAuth } from "@/composables/useAuth";
import { formatRelative } from "@/lib/format";
import { Badge } from "@/components/ui/badge";
import { Resolution } from "@/components/ui/resolution";
import { EmptyState } from "@/components/ui/empty-state";
import { packagePath } from "@/config/navigation";
import HomeSection from "./HomeSection.vue";

/**
 * Enough to recognise the shape of what a machine is doing, not an audit log.
 * The full history is `/api/v1/me/downloads?limit=`, and the admin trail is
 * `/admin/observability/audit-log` — this is the glance.
 */
const SHOWN = 8;

const { t } = useI18n();
const { token } = useAuth();

const { data, error, loading } = useApi<MyDownloadDto[]>(
  () => myDownloads({ query: { limit: SHOWN } }) as Promise<{ data?: unknown; error?: unknown }>,
  [token],
);

const pulls = computed<MyDownloadDto[]>(() => data.value ?? []);
</script>

<template>
  <HomeSection
    heading-id="recent-pulls-heading"
    :title="t('recentPullsWidget.title')"
    :loading="loading"
    :error="error"
  >
    <!--
      A real answer, not a blank. "Nothing yet" on this widget is genuinely
      informative for a developer who thinks their tooling is pointed here and
      has not checked — which is the most common reason a registry proxy
      appears to be doing nothing.
    -->
    <EmptyState
      v-if="!pulls.length"
      :title="t('recentPullsWidget.nothingYet')"
      :description="t('recentPullsWidget.nothingYetBody')"
      data-testid="recent-pulls-empty"
    />

    <ul v-else class="divide-y divide-border border border-border">
      <li
        v-for="pull in pulls"
        :key="`${pull.registry}/${pull.name}/${pull.version}/${pull.artifact ?? ''}/${pull.downloaded_at}`"
        class="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1 px-4 py-2"
      >
        <RouterLink
          :to="packagePath(pull.registry, pull.name)"
          class="min-w-0 font-mono text-sm text-primary hover:underline"
        >
          {{ pull.name }}<span class="text-muted-foreground">@{{ pull.version }}</span>
        </RouterLink>
        <div class="flex items-center gap-2">
          <!-- Retroactive, always: this endpoint returns successful downloads
               only, so a blocked row means something you already have was
               refused after you took it. Worth stating on the surface a
               non-admin actually opens — the catalog carries the same fact, but
               only if you go looking for the package (RFC 0004-bis §5/A9). -->
          <Resolution
            v-if="pull.blocked"
            state="blocked"
            :label="t('common.blocked')"
            :title="t('recentPullsWidget.blockedSincePull')"
          />
          <Badge variant="outline" class="text-xs">{{ pull.registry }}</Badge>
          <!-- `title` carries the absolute time; the relative one is what a
               reader is actually asking for at a glance. -->
          <span class="font-mono text-xs text-muted-foreground" :title="pull.downloaded_at">
            {{ formatRelative(pull.downloaded_at) }}
          </span>
        </div>
      </li>
    </ul>
  </HomeSection>
</template>
