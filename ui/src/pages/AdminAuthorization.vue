<script setup lang="ts">
/**
 * RFC 0015 §4.8 — one page that shows what authorization did.
 *
 * Every mechanism RFC 0015 removes had its own way of being invisible: ownership
 * lived in a table nobody rendered, `bypass_roles` was a field inside a rule, the
 * beta channel was a config block, and the only way to answer "why was this
 * refused?" was to read Rust. A single model deserves a single place to watch it.
 *
 * # Why these five panels are one page
 *
 * §4.8 is explicit, and it is the argument for the page rather than for five
 * pages: *"Three of those five are the fail-open or destructive directions of
 * features decided elsewhere in this document. They are on one page on purpose:
 * a shadowed grant, a self-approved exemption and a retention run about to go
 * live are each individually easy to forget, and collectively they are the list
 * of everything currently trusting an operator to remember."*
 *
 * So the order is deliberate. **Shadow first**, because it is the only one that
 * is actively serving requests the model refuses. Then Exemptions, then Explain.
 * Recent denials sits below the fold because it is diagnostic rather than
 * standing risk — an operator arrives at it from a ticket, not from a review.
 */
import { useI18n } from "vue-i18n";
import { computed, onMounted, ref } from "vue";
import { RouterLink, useRoute } from "vue-router";

import { adminAuthzExplain, authzShadow, listExemptions, auditLog } from "@/client/sdk.gen";
import type { ExplainResponse, ShadowResponse, ExemptionListEntry } from "@/client/types.gen";
import { extractMessage } from "@/composables/useApi";
import SectionTabs from "@/components/admin/SectionTabs.vue";
import { SECURITY_TABS } from "@/config/adminSections";
import { PageHeader } from "@/components/ui/page-header";
import { Select } from "@/components/ui/select";
import { Combobox } from "@/components/ui/combobox";
import { useRegistryOptions } from "@/composables/useRegistryOptions";
import { usePackageNameSuggestions, useVersionSuggestions } from "@/composables/useSuggestions";
import { Button } from "@/components/ui/button";

const { t } = useI18n();
const route = useRoute();
const { options: registryOptions } = useRegistryOptions();

// ── Explain ──────────────────────────────────────────────────────────────────

const registry = ref("");
const subject = ref("role:user");
const action = ref("releases:read");
const packageName = ref("");
const version = ref("");

const packageSuggestions = usePackageNameSuggestions(packageName, registry);
const versionSuggestions = useVersionSuggestions(registry, packageName);

const explained = ref<ExplainResponse | null>(null);
const explainError = ref<string | null>(null);
const explaining = ref(false);

/**
 * The verbs §4.2 defines, as a closed list.
 *
 * A free-text field here would let an operator ask about a verb that does not
 * exist and receive a `400` they have to read carefully — the closed enum is the
 * whole point of phase 1, and the picker is that decision reaching the surface.
 */
const ACTIONS = [
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
  "stats:read",
  "audit:read",
  "audit:purge",
];

/**
 * The subject forms §4.3 defines.
 *
 * A picker rather than free text for the *shape*, with the name typed in: the
 * five forms are closed and the names are not. `token:` is absent because no
 * principal is a machine token yet, and the endpoint refuses it rather than
 * inventing a caller to answer about.
 */
const SUBJECT_FORMS = ["*", "role:anonymous", "role:user", "role:admin"];

async function explain() {
  explaining.value = true;
  explainError.value = null;
  explained.value = null;
  try {
    const res = await adminAuthzExplain({
      query: {
        registry: registry.value,
        subject: subject.value,
        action: action.value,
        ...(packageName.value ? { package: packageName.value } : {}),
        ...(version.value ? { version: version.value } : {}),
      } as never,
    });
    if (res.error) {
      explainError.value = extractMessage(res.error);
      return;
    }
    explained.value = res.data ?? null;
  } catch (e: unknown) {
    explainError.value = e instanceof Error ? e.message : String(e);
  } finally {
    explaining.value = false;
  }
}

// ── Shadow (§4.7) ────────────────────────────────────────────────────────────

const shadowReport = ref<ShadowResponse | null>(null);
const shadowError = ref<string | null>(null);

async function loadShadow() {
  shadowError.value = null;
  try {
    const res = await authzShadow({ query: {} as never });
    if (res.error) {
      shadowError.value = extractMessage(res.error);
      return;
    }
    shadowReport.value = res.data ?? null;
  } catch (e: unknown) {
    shadowError.value = e instanceof Error ? e.message : String(e);
  }
}

// ── Exemptions (§4.5) ────────────────────────────────────────────────────────

const exemptions = ref<ExemptionListEntry[]>([]);
const exemptionsError = ref<string | null>(null);
/** §4.8 asks the panel for this filter by name: *show me every exemption nobody else looked at.* */
const selfApprovedOnly = ref(false);

const shownExemptions = computed(() =>
  selfApprovedOnly.value ? exemptions.value.filter((e) => e.self_approved) : exemptions.value,
);

async function loadExemptions() {
  exemptionsError.value = null;
  if (!registry.value) {
    exemptions.value = [];
    return;
  }
  try {
    const res = await listExemptions({ path: { registry: registry.value } as never });
    if (res.error) {
      exemptionsError.value = extractMessage(res.error);
      return;
    }
    exemptions.value = res.data ?? [];
  } catch (e: unknown) {
    exemptionsError.value = e instanceof Error ? e.message : String(e);
  }
}

// ── Recent denials ───────────────────────────────────────────────────────────

type AuditRow = {
  timestamp?: string;
  user_id?: string | null;
  package_id?: string | null;
  action?: string;
  reason?: string | null;
};

const denials = ref<AuditRow[]>([]);
const denialsError = ref<string | null>(null);

async function loadDenials() {
  denialsError.value = null;
  try {
    // The audit log already records every denial that *happened*; this panel is
    // that list, filtered. Deliberately a different source from Shadow above:
    // one is what was refused, the other is what would have been.
    const res = await auditLog({ query: { result: "denied", limit: 25 } as never });
    if (res.error) {
      denialsError.value = extractMessage(res.error);
      return;
    }
    const data = res.data as unknown as { events?: AuditRow[] } | AuditRow[] | null;
    denials.value = Array.isArray(data) ? data : (data?.events ?? []);
  } catch (e: unknown) {
    denialsError.value = e instanceof Error ? e.message : String(e);
  }
}

onMounted(async () => {
  const one = (v: unknown) => (typeof v === "string" ? v : "");
  registry.value = one(route.query.registry) || registry.value;
  packageName.value = one(route.query.package) || packageName.value;
  version.value = one(route.query.version) || version.value;
  if (one(route.query.subject)) subject.value = one(route.query.subject);
  if (one(route.query.action)) action.value = one(route.query.action);

  // The three standing-risk panels load without being asked. An operator who
  // has to press a button to discover a shadow-mode node is serving everything has a page
  // that does not do its job.
  await Promise.all([loadShadow(), loadDenials(), loadExemptions()]);
});
</script>

<template>
  <div class="space-y-6">
    <SectionTabs :tabs="SECURITY_TABS" />
    <PageHeader
      variant="display"
      :title="t('adminAuthorization.title')"
      :description="t('adminAuthorization.description')"
    />

    <!-- ── Shadow ──────────────────────────────────────────────────────────
         First, and the only panel that can be *currently wrong*: a shadowed
         node serves what the model refuses. -->
    <section class="space-y-3" data-testid="panel-shadow">
      <h2 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground">
        {{ t("adminAuthorization.shadowTitle") }}
      </h2>
      <p v-if="shadowError" class="text-sm text-destructive">{{ shadowError }}</p>
      <p
        v-else-if="shadowReport?.no_shadow_configured"
        class="rounded-sm border border-border bg-muted/30 p-3 text-sm"
      >
        {{ t("adminAuthorization.shadowNoneConfigured") }}
      </p>
      <p
        v-else-if="!shadowReport?.by_node?.length"
        class="rounded-sm border border-border bg-muted/30 p-3 text-sm"
      >
        {{ t("adminAuthorization.shadowQuiet") }}
      </p>
      <div v-else class="space-y-2">
        <p class="rounded-sm border border-destructive/40 bg-destructive/10 p-3 text-sm">
          {{ t("adminAuthorization.shadowWarning") }}
        </p>
        <div class="overflow-x-auto">
          <table class="w-full text-sm">
            <thead class="text-left text-xs uppercase text-muted-foreground">
              <tr>
                <th class="py-1 pr-4">{{ t("adminAuthorization.node") }}</th>
                <th class="py-1 pr-4">{{ t("adminAuthorization.until") }}</th>
                <th class="py-1 pr-4">{{ t("adminAuthorization.served") }}</th>
                <th class="py-1 pr-4">{{ t("adminAuthorization.missingVerbs") }}</th>
                <th class="py-1">{{ t("adminAuthorization.subjects") }}</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="row in shadowReport.by_node"
                :key="row.node"
                class="border-t border-border"
              >
                <td class="py-1 pr-4 font-mono text-xs">{{ row.node }}</td>
                <td class="py-1 pr-4">{{ row.shadow_until }}</td>
                <td class="py-1 pr-4 tabular-nums">{{ row.count }}</td>
                <td class="py-1 pr-4 font-mono text-xs">{{ row.actions.join(", ") }}</td>
                <td class="py-1 font-mono text-xs">{{ row.subjects.join(", ") }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>
    </section>

    <!-- ── Exemptions ───────────────────────────────────────────────────── -->
    <section class="space-y-3" data-testid="panel-exemptions">
      <div class="flex items-center justify-between">
        <h2 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground">
          {{ t("adminAuthorization.exemptionsTitle") }}
        </h2>
        <label class="flex items-center gap-2 text-sm">
          <input v-model="selfApprovedOnly" type="checkbox" />
          {{ t("adminAuthorization.selfApprovedOnly") }}
        </label>
      </div>
      <p v-if="exemptionsError" class="text-sm text-destructive">{{ exemptionsError }}</p>
      <p v-else-if="!registry" class="text-sm text-muted-foreground">
        {{ t("adminAuthorization.exemptionsNeedRegistry") }}
      </p>
      <p
        v-else-if="!shownExemptions.length"
        class="rounded-sm border border-border bg-muted/30 p-3 text-sm"
      >
        {{ t("adminAuthorization.exemptionsNone") }}
      </p>
      <div v-else class="overflow-x-auto">
        <table class="w-full text-sm">
          <thead class="text-left text-xs uppercase text-muted-foreground">
            <tr>
              <th class="py-1 pr-4">{{ t("common.package") }}</th>
              <th class="py-1 pr-4">{{ t("common.version") }}</th>
              <th class="py-1 pr-4">{{ t("adminAuthorization.gate") }}</th>
              <th class="py-1 pr-4">{{ t("adminAuthorization.until") }}</th>
              <th class="py-1">{{ t("adminAuthorization.reason") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(e, i) in shownExemptions" :key="i" class="border-t border-border">
              <td class="py-1 pr-4 font-mono text-xs">{{ e.package }}</td>
              <td class="py-1 pr-4 font-mono text-xs">{{ e.version }}</td>
              <td class="py-1 pr-4 font-mono text-xs">{{ e.gate }}</td>
              <td class="py-1 pr-4">
                {{ e.exempt_until }}
                <span v-if="e.expired" class="ml-1 text-xs text-muted-foreground">{{
                  t("adminAuthorization.expired")
                }}</span>
              </td>
              <td class="py-1">
                {{ e.reason }}
                <span v-if="e.self_approved" class="ml-2 text-xs text-copper">{{
                  t("adminAuthorization.selfApproved")
                }}</span>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>

    <!-- ── Explain ──────────────────────────────────────────────────────── -->
    <section class="space-y-3" data-testid="panel-explain">
      <h2 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground">
        {{ t("adminAuthorization.explainTitle") }}
      </h2>

      <form class="space-y-4 max-w-2xl" @submit.prevent="explain">
        <div class="grid grid-cols-2 gap-4">
          <div class="space-y-1">
            <label for="az-registry" class="text-sm font-medium">{{ t("common.registry") }}</label>
            <Select
              id="az-registry"
              v-model="registry"
              :options="registryOptions"
              :placeholder="t('adminHealth.chooseRegistry')"
              @update:model-value="loadExemptions"
            />
          </div>
          <div class="space-y-1">
            <label for="az-action" class="text-sm font-medium">{{
              t("adminAuthorization.verb")
            }}</label>
            <select
              id="az-action"
              v-model="action"
              class="w-full rounded-sm border border-border bg-background px-3 py-1.5 text-sm"
            >
              <option v-for="a in ACTIONS" :key="a" :value="a">{{ a }}</option>
            </select>
          </div>
          <div class="space-y-1">
            <label for="az-subject" class="text-sm font-medium">{{
              t("adminAuthorization.subject")
            }}</label>
            <!-- Free text with the closed forms offered: the five *shapes* are
                 closed and the names inside them are not, so a picker alone
                 could not express `group:oidc1:eng` or `user:alice`. -->
            <Combobox
              id="az-subject"
              v-model="subject"
              :options="SUBJECT_FORMS.map((s) => ({ value: s, label: s }))"
              placeholder="role:user"
            />
          </div>
          <div class="space-y-1">
            <label for="az-package" class="text-sm font-medium">{{
              t("adminAccessCheck.packageName")
            }}</label>
            <Combobox
              id="az-package"
              v-model="packageName"
              :options="packageSuggestions.options.value"
              :loading="packageSuggestions.loading.value"
              placeholder="lodash"
            />
          </div>
          <div class="space-y-1">
            <label for="az-version" class="text-sm font-medium">{{ t("common.version") }}</label>
            <Combobox
              id="az-version"
              v-model="version"
              :options="versionSuggestions.options.value"
              :loading="versionSuggestions.loading.value"
              :disabled="!versionSuggestions.ready()"
              :disabled-reason="t('adminAccessCheck.versionNeedsPackage')"
              placeholder="1.0.0"
            />
          </div>
        </div>

        <Button type="submit" :disabled="explaining || !registry">
          {{ explaining ? t("common.loading") : t("adminAuthorization.explainAction") }}
        </Button>
      </form>

      <p v-if="explainError" class="text-sm text-destructive">{{ explainError }}</p>

      <div v-if="explained" class="space-y-3" data-testid="explain-result">
        <p class="text-sm font-semibold">
          <span :class="explained.decision === 'allow' ? 'text-foreground' : 'text-destructive'">{{
            explained.decision.toUpperCase()
          }}</span>
          <span v-if="explained.reason" class="ml-2 font-normal text-muted-foreground">{{
            explained.reason
          }}</span>
        </p>

        <!-- §4.7, and the thing an operator must not miss: under shadow-mode the
             grants refuse and the server serves anyway. A `DENY` read without
             this is the opposite of what happens. -->
        <p
          v-if="explained.shadowed_by"
          class="rounded-sm border border-destructive/40 bg-destructive/10 p-3 text-sm"
          data-testid="explain-shadow-note"
        >
          {{
            t("adminAuthorization.explainShadowed", {
              node: explained.shadowed_by.node,
              until: explained.shadowed_by.until,
            })
          }}
        </p>

        <div v-if="explained.resolved.length" class="overflow-x-auto">
          <table class="w-full text-sm">
            <thead class="text-left text-xs uppercase text-muted-foreground">
              <tr>
                <!-- `Granted by` is the point: a resolved set without provenance
                     says what a subject holds; the tier says which line to
                     edit. -->
                <th class="py-1 pr-4">{{ t("adminAuthorization.verb") }}</th>
                <th class="py-1 pr-4">{{ t("adminAuthorization.grantedBy") }}</th>
                <th class="py-1">{{ t("adminAuthorization.matchedSubject") }}</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="(v, i) in explained.resolved" :key="i" class="border-t border-border">
                <td class="py-1 pr-4 font-mono text-xs">{{ v.action }}</td>
                <td class="py-1 pr-4 font-mono text-xs">{{ v.granted_by }}</td>
                <td class="py-1 font-mono text-xs">{{ v.subject }}</td>
              </tr>
            </tbody>
          </table>
        </div>
        <p v-else class="text-sm text-muted-foreground">
          {{ t("adminAuthorization.noVerbsResolved") }}
        </p>

        <!-- §4.5's other direction. A resolved set that showed `releases:read`
             without saying the package is `team`-visible answers half the
             question and reads as the whole one. -->
        <dl class="grid grid-cols-2 gap-x-6 gap-y-1 text-sm sm:grid-cols-4">
          <div>
            <dt class="text-xs text-muted-foreground">visibility</dt>
            <dd class="font-mono text-xs">{{ explained.attributes.visibility }}</dd>
          </div>
          <div>
            <dt class="text-xs text-muted-foreground">prerelease_visibility</dt>
            <dd class="font-mono text-xs">{{ explained.attributes.prerelease_visibility }}</dd>
          </div>
          <div>
            <dt class="text-xs text-muted-foreground">immutable</dt>
            <dd class="font-mono text-xs">{{ explained.attributes.immutable }}</dd>
          </div>
          <div>
            <dt class="text-xs text-muted-foreground">monotonic</dt>
            <dd class="font-mono text-xs">{{ explained.attributes.monotonic }}</dd>
          </div>
        </dl>
        <p
          v-if="explained.attributes.exempt_gates?.length"
          class="text-sm"
          data-testid="explain-exempt-gates"
        >
          {{
            t("adminAuthorization.explainExemptGates", {
              gates: explained.attributes.exempt_gates.join(", "),
            })
          }}
        </p>

        <p class="text-xs text-muted-foreground">
          {{ t("adminAuthorization.tiersWalked") }}: {{ explained.tiers_walked.join(" → ") }}
        </p>

        <!-- Always, including on an ALLOW, which is the answer it most changes
             the meaning of: a bare verdict is ambiguous between "nothing denies
             this" and "nothing I looked at denies this". -->
        <details class="text-xs text-muted-foreground">
          <summary class="cursor-pointer">{{ t("adminAuthorization.notCovered") }}</summary>
          <ul class="mt-1 list-disc pl-5">
            <li v-for="(layer, i) in explained.not_covered" :key="i">{{ layer }}</li>
          </ul>
        </details>
      </div>
    </section>

    <!-- ── Recent denials ───────────────────────────────────────────────── -->
    <section class="space-y-3" data-testid="panel-denials">
      <h2 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground">
        {{ t("adminAuthorization.denialsTitle") }}
      </h2>
      <p v-if="denialsError" class="text-sm text-destructive">{{ denialsError }}</p>
      <p
        v-else-if="!denials.length"
        class="rounded-sm border border-border bg-muted/30 p-3 text-sm"
      >
        {{ t("adminAuthorization.denialsNone") }}
      </p>
      <div v-else class="overflow-x-auto">
        <table class="w-full text-sm">
          <thead class="text-left text-xs uppercase text-muted-foreground">
            <tr>
              <th class="py-1 pr-4">{{ t("adminAuthorization.when") }}</th>
              <th class="py-1 pr-4">{{ t("adminAuthorization.subject") }}</th>
              <th class="py-1 pr-4">{{ t("adminAuthorization.coordinate") }}</th>
              <th class="py-1">{{ t("adminAuthorization.reason") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(d, i) in denials" :key="i" class="border-t border-border">
              <td class="py-1 pr-4 text-xs">{{ d.timestamp }}</td>
              <td class="py-1 pr-4 font-mono text-xs">{{ d.user_id ?? "-" }}</td>
              <td class="py-1 pr-4 font-mono text-xs">{{ d.package_id ?? "-" }}</td>
              <td class="py-1 text-xs">{{ d.reason ?? "-" }}</td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>

    <!-- ── Retention ────────────────────────────────────────────────────────
         The fifth panel, and a link rather than a table: RFC 0016 already has a
         page that runs and renders the dry run, and duplicating it here would
         be a second place the report is read from. What §4.8 wants on *this*
         page is that the destructive direction is not out of sight, which the
         pointer serves. -->
    <section class="space-y-2" data-testid="panel-retention">
      <h2 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground">
        {{ t("adminAuthorization.retentionTitle") }}
      </h2>
      <p class="text-sm text-muted-foreground">
        {{ t("adminAuthorization.retentionBlurb") }}
        <RouterLink to="/admin/packages/all" class="underline">{{
          t("adminAuthorization.retentionLink")
        }}</RouterLink>
      </p>
    </section>
  </div>
</template>
