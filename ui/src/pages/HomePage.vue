<script setup lang="ts">
import { useI18n } from "vue-i18n";
/**
 * The one route that adapts to who is asking (RFC 0003 §4.3).
 *
 * `/` used to redirect blindly to `/packages`, which meant a freshly deployed
 * instance greeted its operator with an empty table and no next action. The
 * three audiences arriving here want different things, and the instance's own
 * state decides which of them is being served:
 *
 *   - fresh instance + admin  → what is configured, what is missing, what to do
 *   - anyone else             → straight to work, no ceremony
 *
 * The probe is one call the catalog already makes. It never *writes* config —
 * first-run guidance explains and links; it does not gain a privileged endpoint.
 */
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { listRegistries } from "@/client/sdk.gen";
import type { RegistryInfo } from "@/client/types.gen";
import { useApi } from "@/composables/useApi";
import { roleLabel } from "@/lib/roles";
import { useAuth } from "@/composables/useAuth";
import { DOCS_URL } from "@/config";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { Skeleton } from "@/components/ui/skeleton";
import { AsyncState } from "@/components/ui/async-state";
import AdvisoriesWidget from "@/components/home/AdvisoriesWidget.vue";
import QuotaWidget from "@/components/home/QuotaWidget.vue";
import RecentPullsWidget from "@/components/home/RecentPullsWidget.vue";

const { t } = useI18n();

const { identity, isAdmin, isAuthenticated, token } = useAuth();

const { data, error, loading, reload } = useApi<RegistryInfo[]>(
  () => listRegistries() as Promise<{ data?: unknown; error?: unknown }>,
  [token],
);

const registries = computed<RegistryInfo[]>(() => data.value ?? []);
const isFresh = computed(() => !loading.value && !error.value && registries.value.length === 0);
const publishable = computed(() => registries.value.filter((r) => r.mode !== "proxy").length);

/**
 * The instance in one ruled line, in the catalog caption's grammar. It was a
 * three-cell stat row at 24px — the stat-tile row DESIGN refuses by name — and
 * its third cell printed the wire role, `anonymous`, in English on a French
 * session.
 */
const facts = computed(() => [
  t("homePage.registryCount", registries.value.length),
  t("homePage.publishableCount", publishable.value),
  t("homePage.youAre", { role: roleLabel(t, identity.value?.role) }),
]);
const name = computed(() => identity.value?.user_id ?? "");
const greeting = computed(() =>
  name.value ? t("home.greeting", { user: name.value }) : t("home.greetingNoName"),
);

/**
 * Whether the greeting can hold the Display step.
 *
 * `user_id` is whatever the provider's `user_id_claim` says, and that defaults
 * to `sub` — a 36-character UUID on Keycloak. Silkscreen's advance is ~0.8em,
 * so at 104px a UUID greeting is four lines of poster and the dashboard starts
 * below the fold. The step down is to Pixel Medium, which is on the ramp.
 * ponytail: a length threshold, not a measurement — swap for a fit measurement
 * if a name lands just the wrong side of it.
 */
const displayFits = computed(() => greeting.value.length <= 24);
</script>

<template>
  <div class="space-y-6">
    <header class="space-y-3">
      <!-- The Display step, which DESIGN.md gives one element per view: the
           wordmark used to hold it here, three centimetres under the identical
           wordmark in the bar above. The greeting is what this route actually
           says that no other route does, so it takes the poster.
           No `uppercase`, unlike the catalog's: Silkscreen has no lowercase
           glyphs, so the class would only be telling the browser something the
           face already decided.

           `text-display` is 56px, 72px from 640 — the console's one page-title
           step, shared with the catalog, the package sheet and the setup guide.
           It was a four-step ramp topping out at 104px until this greeting
           showed what that costs a title that is not a single short word. -->
      <h1
        class="font-display font-bold tracking-[0.02em] leading-[0.92] break-words"
        :class="displayFits ? 'text-display' : 'text-2xl'"
      >
        {{ greeting }}
      </h1>
      <p v-if="!isAuthenticated" class="text-sm text-muted-foreground">
        {{ t("homePage.aCacheAndRegistry") }}
      </p>
    </header>

    <Skeleton v-if="loading" :lines="4" />

    <!-- What failed and a retry — and the quick links below still render: a
         failed list was a dead end that hid the two things people come for. -->
    <AsyncState
      v-else-if="error"
      :loading="false"
      :error="error"
      :title="t('homePage.registriesCouldNotLoad')"
      :on-retry="reload"
    />

    <!-- Fresh instance. The operator is the only one who can act on this, so
         only they are told how; everyone else is told what it means for them. -->
    <EmptyState
      v-else-if="isFresh"
      :title="isAdmin ? t('adminDashboard.noRegistriesConfiguredYet') : t('home.freshTitleOther')"
      :description="isAdmin ? t('home.freshBodyAdmin') : t('home.freshBodyOther')"
    >
      <template v-if="isAdmin" #action>
        <Button as-child size="sm">
          <RouterLink to="/admin/operations/config-reload">{{
            t("homePage.openConfig")
          }}</RouterLink>
        </Button>
        <Button as-child size="sm" variant="outline">
          <a :href="DOCS_URL" target="_blank" rel="noopener noreferrer">{{
            t("homePage.configurationGuide")
          }}</a>
        </Button>
      </template>
    </EmptyState>

    <!-- Working instance: the shortest path to the two things people come for. -->
    <template v-if="!loading && !isFresh">
      <p
        v-if="!error"
        class="border-y border-dashed border-rule-soft py-2 font-mono text-sm text-muted-foreground"
      >
        {{ facts.join(" · ") }}
      </p>

      <div class="flex flex-wrap gap-2">
        <Button as-child size="sm">
          <!-- `/packages`, not `/explore`. `/explore` is a legacy address kept
               alive by a `beforeEach` guard for old bookmarks and links in
               `docs/`; it is not a *route*, so `RouterLink` could not resolve
               it and logged `[VUE_ROUTER_R0004] No match found` on every render
               of this page. The click still worked — the guard caught it — which
               is why a broken link sat on the home page looking fine.

               It is also the rule `navigation.ts` states for the nav bar, one
               level in: nothing the console links to should land on a redirect.
               Incoming legacy URLs are what the guard is for. -->
          <RouterLink to="/packages">{{ t("homePage.browsePackages") }}</RouterLink>
        </Button>
        <Button as-child size="sm" variant="outline">
          <RouterLink to="/setup">{{ t("homePage.pointAToolAt") }}</RouterLink>
        </Button>
        <Button v-if="isAuthenticated" as-child size="sm" variant="outline">
          <RouterLink to="/me/namespace">{{ t("homePage.myNamespace") }}</RouterLink>
        </Button>
        <Button v-if="isAdmin" as-child size="sm" variant="outline">
          <RouterLink to="/admin/dashboard">{{ t("common.admin") }}</RouterLink>
        </Button>
      </div>

      <!-- Both are about *you*, so an anonymous viewer gets neither: there is
           no quota and no pull history attached to nobody, and rendering an
           empty one would invite them to read it as "you have none"
           (RFC 0004 §4.2). -->
      <template v-if="isAuthenticated">
        <QuotaWidget />
        <!-- Before advisories: "what did I pull" is the ordinary question and
             is always answerable, where "is any of it known-bad" is silent
             most of the time. A widget that says nothing most days should not
             be the one above the one that always says something. -->
        <RecentPullsWidget />
        <AdvisoriesWidget />
      </template>
    </template>
  </div>
</template>
