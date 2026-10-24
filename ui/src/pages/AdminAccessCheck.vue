<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { computed, onMounted, ref } from "vue";
import { useRoute, RouterLink } from "vue-router";
import { adminAccessCheck } from "@/client/sdk.gen";
import type { AccessSimulationResponse } from "@/client/types.gen";
import { extractMessage } from "@/composables/useApi";
import SectionTabs from "@/components/admin/SectionTabs.vue";
import { SECURITY_TABS } from "@/config/adminSections";
import { PageHeader } from "@/components/ui/page-header";
import { Select } from "@/components/ui/select";
import { Combobox } from "@/components/ui/combobox";
import { useRegistryOptions } from "@/composables/useRegistryOptions";
import {
  usePackageNameSuggestions,
  useSubjectSuggestions,
  useVersionSuggestions,
} from "@/composables/useSuggestions";

import { Button } from "@/components/ui/button";
const { t } = useI18n();

/** RFC 0004-bis §6.2: the registry set is closed, small and already fetched. */
const { options: registryOptions } = useRegistryOptions();
const route = useRoute();

const registry = ref("");
const packageName = ref("");
const version = ref("1.0.0");
const resourceType = ref("releases:read");
const userId = ref("");
const role = ref("anonymous");
const groups = ref("");
const clientIp = ref("");

/**
 * The three suggesting fields (RFC 0004-bis §6.2).
 *
 * Every one still accepts a value the instance has never seen — the simulator's
 * whole point is answering about a coordinate that may not be cached, so a
 * field that only took what it could offer would break it.
 */
const packageSuggestions = usePackageNameSuggestions(packageName, registry);
const versionSuggestions = useVersionSuggestions(registry, packageName);
const subjectSuggestions = useSubjectSuggestions(userId);

const result = ref<AccessSimulationResponse | null>(null);
const loading = ref(false);
const error = ref<string | null>(null);

/** The layers the answer did not account for, named so the gap is visible. */
const uncovered = computed(() => {
  const covers = result.value?.covers;
  if (!covers) return [];
  return [
    !covers.account_blocks && t("adminAccessCheck.layerAccountBlocks"),
    !covers.ip_blocks && t("adminAccessCheck.layerIpBlocks"),
  ].filter((v): v is string => typeof v === "string");
});

/**
 * The verbs this page can simulate, in the order the vocabulary lists them.
 *
 * The set is closed: the endpoint parses `resource_type` into an `Action` and
 * answers `400` for anything else. This list carried `releases:write` and
 * `source:write`, neither of which has ever existed — two of the four options
 * could only ever produce a bad request. Source of truth is `Action::ALL` in
 * `crates/core/src/entities/permission.rs`, tabulated in
 * `docs/guide/access-control.md#verbs`; the instance-tier verbs (`stats:read`,
 * `audit:*`, the control surfaces) are left out because they name no coordinate
 * and this page asks about one.
 */
const RESOURCE_TYPES = [
  "releases:read",
  "releases:list",
  "releases:publish",
  "releases:overwrite",
  "releases:yank",
  "releases:delete",
  "source:read",
  "catalogue:browse",
  "owners:read",
  "owners:write",
  "packages:block",
  "gates:exempt",
];

/**
 * Prefill from the query, as `/tools/access-check` already does.
 *
 * "Nobody opens an access checker for fun; they open it because something was
 * refused" — the project's own tested premise for the public checker. This page
 * had seven empty fields and read no query at all, so an operator who had just
 * seen a denial in the audit log retyped every coordinate by hand.
 */
onMounted(() => {
  const q = route.query;
  const one = (v: unknown) => (typeof v === "string" ? v : "");
  registry.value = one(q.registry) || registry.value;
  packageName.value = one(q.name) || packageName.value;
  version.value = one(q.version) || version.value;
  userId.value = one(q.user_id) || userId.value;
  role.value = one(q.role) || role.value;
  clientIp.value = one(q.client_ip) || clientIp.value;
});

async function simulate() {
  loading.value = true;
  error.value = null;
  result.value = null;
  try {
    const body: Record<string, unknown> = {
      registry: registry.value,
      package_name: packageName.value,
      version: version.value,
      resource_type: resourceType.value,
      role: role.value || "anonymous",
    };
    if (userId.value) body.user_id = userId.value;
    if (clientIp.value) body.client_ip = clientIp.value;
    const grps = groups.value
      .split(",")
      .map((g) => g.trim())
      .filter(Boolean);
    if (grps.length) body.groups = grps;

    // The generated client, not a bare relative `fetch`. There is no `/api`
    // proxy in front of the SPA (CLAUDE.md), so `fetch("/api/v1/...")` POSTed
    // to the Vite origin on both dev servers and on every deployment where the
    // API is not same-origin. `adminAccessCheck` was already generated and
    // typed, and carries the base URL and the auth header for free.
    const res = await adminAccessCheck({ body: body as never });
    if (res.error) {
      error.value = extractMessage(res.error);
      return;
    }
    result.value = res.data ?? null;
  } catch (e: unknown) {
    error.value = e instanceof Error ? e.message : String(e);
  } finally {
    loading.value = false;
  }
}
</script>

<template>
  <div class="space-y-6">
    <SectionTabs :tabs="SECURITY_TABS" />
    <PageHeader
      variant="display"
      :title="t('adminAccessCheck.rbacAccessCheck')"
      :description="t('adminAccessCheck.simulateWhetherAnIdentityWould')"
    />

    <form class="space-y-4 max-w-lg" @submit.prevent="simulate">
      <div class="grid grid-cols-2 gap-4">
        <div class="space-y-1">
          <label for="aac-registry" class="text-sm font-medium">{{ t("common.registry") }}</label>
          <!-- Was a bare `<input>` with hand-rolled border classes rather than
               the `Input` primitive, so it inherited nothing the system does
               next. It is a closed set either way. -->
          <Select
            id="aac-registry"
            v-model="registry"
            :options="registryOptions"
            :placeholder="t('adminHealth.chooseRegistry')"
          />
        </div>
        <div class="space-y-1">
          <label for="aac-package" class="text-sm font-medium">{{
            t("adminAccessCheck.packageName")
          }}</label>
          <Combobox
            id="aac-package"
            v-model="packageName"
            :options="packageSuggestions.options.value"
            :loading="packageSuggestions.loading.value"
            placeholder="lodash"
          />
        </div>
        <div class="space-y-1">
          <label for="aac-version" class="text-sm font-medium">{{ t("common.version") }}</label>
          <!-- Closed once its parents are answered, and disabled with a stated
               reason until then rather than merely dark. -->
          <Combobox
            id="aac-version"
            v-model="version"
            :options="versionSuggestions.options.value"
            :loading="versionSuggestions.loading.value"
            :disabled="!versionSuggestions.ready()"
            :disabled-reason="t('adminAccessCheck.versionNeedsPackage')"
            placeholder="1.0.0"
          />
        </div>
        <div class="space-y-1">
          <label for="aac-resource-type" class="text-sm font-medium">{{
            t("adminAccessCheck.resourceType")
          }}</label>
          <select
            id="aac-resource-type"
            v-model="resourceType"
            class="w-full rounded-sm border border-border bg-background px-3 py-1.5 text-sm"
          >
            <option v-for="rt in RESOURCE_TYPES" :key="rt" :value="rt">{{ rt }}</option>
          </select>
        </div>
      </div>

      <hr class="border-border" />

      <p class="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
        {{ t("adminAccessCheck.simulatedIdentity") }}
      </p>

      <div class="grid grid-cols-2 gap-4">
        <div class="space-y-1">
          <label for="aac-role" class="text-sm font-medium">{{ t("common.role") }}</label>
          <select
            id="aac-role"
            v-model="role"
            class="w-full rounded-sm border border-border bg-background px-3 py-1.5 text-sm"
          >
            <option value="anonymous">anonymous</option>
            <option value="user">user</option>
            <option value="admin">admin</option>
          </select>
        </div>
        <div class="space-y-1">
          <label for="aac-user-id" class="text-sm font-medium"
            >{{ t("adminAccessCheck.userId") }}
            <span class="text-muted-foreground">{{ t("adminAccessCheck.optional") }}</span></label
          >
          <!-- The field whose failure was silent: an operator typing `alice`
               on an instance that stores `oidc:alice` got an answer about a
               subject that does not exist, with nothing to say so (A8). -->
          <Combobox
            id="aac-user-id"
            v-model="userId"
            :options="subjectSuggestions.options.value"
            :loading="subjectSuggestions.loading.value"
            placeholder="alice"
          />
        </div>
        <div class="col-span-2 space-y-1">
          <label for="aac-groups" class="text-sm font-medium"
            >{{ t("common.groups") }}
            <span class="text-muted-foreground">{{
              t("adminAccessCheck.commaSeparated")
            }}</span></label
          >
          <input
            id="aac-groups"
            v-model="groups"
            :placeholder="t('adminAccessCheck.oidc1TeamATeam')"
            class="w-full rounded-sm border border-border bg-background px-3 py-1.5 text-sm"
          />
        </div>
        <!--
          A simulated request has no client address, so the IP block layer can
          only be checked against one you supply. Left blank, the result says so
          rather than answering as though the address were clean — which is the
          same defect one level down (RFC 0004-bis B4).
        -->
        <div class="col-span-2 space-y-1">
          <label for="aac-client-ip" class="text-sm font-medium"
            >{{ t("adminAccessCheck.clientIp") }}
            <span class="text-muted-foreground">{{ t("adminAccessCheck.optional") }}</span></label
          >
          <input
            id="aac-client-ip"
            v-model="clientIp"
            placeholder="10.0.0.7"
            class="w-full rounded-sm border border-border bg-background px-3 py-1.5 text-sm"
          />
          <p class="text-xs text-muted-foreground">
            {{ t("adminAccessCheck.clientIpHelp") }}
          </p>
        </div>
      </div>

      <Button type="submit" :disabled="loading">
        {{ loading ? t("accessCheck.checking") : t("adminAccessCheck.checkAccess") }}
      </Button>
    </form>

    <div
      v-if="error"
      class="rounded-sm border border-destructive/50 px-4 py-3 text-sm text-destructive"
    >
      {{ error }}
    </div>

    <div
      v-if="result"
      class="rounded-sm border px-4 py-3 space-y-1"
      :class="result.decision === 'allow' ? 'border-foreground/50' : 'border-destructive/50'"
    >
      <p
        class="font-semibold text-sm"
        :class="result.decision === 'allow' ? 'text-foreground' : 'text-destructive'"
      >
        {{ result.decision === "allow" ? t("adminAccessCheck.allow") : t("adminAccessCheck.deny") }}
      </p>
      <p v-if="result.reason" class="text-sm text-muted-foreground">{{ result.reason }}</p>
      <p v-if="result.rule_matched" class="text-xs text-muted-foreground">
        {{ t("adminAccessCheck.ruleMatched", { rule: result.rule_matched }) }}
      </p>
      <!--
        Which layer, not just which answer. The endpoint used to evaluate
        registry policy rules and nothing else, so this page could say "allow"
        about an account the next tab showed as blocked; RFC 0004-bis A1 made it
        consult both block stores, and `blocked_by` is how an operator knows
        where to go and lift it.
      -->
      <p v-if="result.blocked_by === 'account'" class="text-xs text-muted-foreground">
        {{ t("adminAccessCheck.blockedByAccount") }}
        <RouterLink to="/admin/security/blocks" class="text-primary hover:underline">{{
          t("adminAccessCheck.seeBlocks")
        }}</RouterLink>
      </p>
      <p v-else-if="result.blocked_by === 'ip'" class="text-xs text-muted-foreground">
        {{ t("adminAccessCheck.blockedByIp") }}
        <RouterLink to="/admin/security/blocks" class="text-primary hover:underline">{{
          t("adminAccessCheck.seeBlocks")
        }}</RouterLink>
      </p>
      <!--
        And what it did *not* look at. An `allow` over two unconsulted layers
        reads identically to an `allow` over three consulted ones, which is the
        shape of every finding in this RFC.
      -->
      <p v-if="uncovered.length" class="border-t border-border pt-2 text-xs text-muted-foreground">
        {{ t("adminAccessCheck.notCovered", { layers: uncovered.join(", ") }) }}
      </p>
    </div>
  </div>
</template>
