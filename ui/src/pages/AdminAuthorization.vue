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
import { Announcer } from "@/components/ui/announcer";
import { Alert } from "@/components/ui/alert";
import { AsyncState } from "@/components/ui/async-state";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";

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
      <h2 class="font-mono text-xs uppercase tracking-[0.16em] text-muted-foreground">
        {{ t("adminAuthorization.shadowTitle") }}
      </h2>
      <!-- "No shadow" and "a quiet shadow" share the empty panel and not its
           words: they mean opposite things. -->
      <AsyncState
        :loading="false"
        :error="shadowError"
        :empty="!shadowReport?.by_node?.length"
        :empty-message="
          shadowReport?.no_shadow_configured
            ? t('adminAuthorization.shadowNoneConfigured')
            : t('adminAuthorization.shadowQuiet')
        "
        :on-retry="loadShadow"
      >
        <div class="space-y-2">
          <Alert variant="destructive">{{ t("adminAuthorization.shadowWarning") }}</Alert>
          <Table :label="t('adminAuthorization.shadowTitle')">
            <TableHeader>
              <TableRow>
                <TableHead>{{ t("adminAuthorization.node") }}</TableHead>
                <TableHead>{{ t("adminAuthorization.until") }}</TableHead>
                <TableHead class="text-right">{{ t("adminAuthorization.served") }}</TableHead>
                <TableHead>{{ t("adminAuthorization.missingVerbs") }}</TableHead>
                <TableHead>{{ t("adminAuthorization.subjects") }}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              <TableRow v-for="row in shadowReport?.by_node" :key="row.node">
                <TableCell class="font-mono text-xs">{{ row.node }}</TableCell>
                <TableCell>{{ row.shadow_until }}</TableCell>
                <TableCell class="text-right tabular-nums">{{ row.count }}</TableCell>
                <TableCell class="font-mono text-xs">{{ row.actions.join(", ") }}</TableCell>
                <TableCell class="font-mono text-xs">{{ row.subjects.join(", ") }}</TableCell>
              </TableRow>
            </TableBody>
          </Table>
        </div>
      </AsyncState>
    </section>

    <!-- ── Exemptions ───────────────────────────────────────────────────── -->
    <section class="space-y-3" data-testid="panel-exemptions">
      <div class="flex items-center justify-between">
        <h2 class="font-mono text-xs uppercase tracking-[0.16em] text-muted-foreground">
          {{ t("adminAuthorization.exemptionsTitle") }}
        </h2>
        <div class="flex items-center gap-2">
          <Switch id="az-self-approved" v-model="selfApprovedOnly" />
          <Label for="az-self-approved">{{ t("adminAuthorization.selfApprovedOnly") }}</Label>
        </div>
      </div>
      <AsyncState
        :loading="false"
        :error="exemptionsError"
        :empty="!registry || !shownExemptions.length"
        :empty-message="
          registry
            ? t('adminAuthorization.exemptionsNone')
            : t('adminAuthorization.exemptionsNeedRegistry')
        "
        :on-retry="loadExemptions"
      >
        <Table :label="t('adminAuthorization.exemptionsTitle')">
          <TableHeader>
            <TableRow>
              <TableHead>{{ t("common.package") }}</TableHead>
              <TableHead>{{ t("common.version") }}</TableHead>
              <TableHead>{{ t("adminAuthorization.gate") }}</TableHead>
              <TableHead>{{ t("adminAuthorization.until") }}</TableHead>
              <TableHead>{{ t("adminAuthorization.reason") }}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <TableRow v-for="(e, i) in shownExemptions" :key="i">
              <TableCell class="font-mono text-xs">{{ e.package }}</TableCell>
              <TableCell class="font-mono text-xs">{{ e.version }}</TableCell>
              <TableCell class="font-mono text-xs">{{ e.gate }}</TableCell>
              <TableCell>
                {{ e.exempt_until }}
                <span v-if="e.expired" class="ml-1 text-xs text-muted-foreground">{{
                  t("adminAuthorization.expired")
                }}</span>
              </TableCell>
              <TableCell>
                {{ e.reason }}
                <span v-if="e.self_approved" class="ml-2 text-xs text-copper">{{
                  t("adminAuthorization.selfApproved")
                }}</span>
              </TableCell>
            </TableRow>
          </TableBody>
        </Table>
      </AsyncState>
    </section>

    <!-- ── Explain ──────────────────────────────────────────────────────── -->
    <section class="space-y-3" data-testid="panel-explain">
      <h2 class="font-mono text-xs uppercase tracking-[0.16em] text-muted-foreground">
        {{ t("adminAuthorization.explainTitle") }}
      </h2>

      <form class="space-y-4 max-w-2xl" @submit.prevent="explain">
        <div class="grid grid-cols-1 sm:grid-cols-2 gap-4">
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
            <Select
              id="az-action"
              v-model="action"
              :options="ACTIONS.map((a) => ({ value: a, label: a }))"
            />
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

      <p v-if="explainError" role="alert" class="text-sm text-destructive">{{ explainError }}</p>

      <Announcer
        :message="
          explained
            ? [explained.decision.toUpperCase(), explained.reason].filter(Boolean).join('. ')
            : ''
        "
      />
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
        <Alert v-if="explained.shadowed_by" variant="destructive" data-testid="explain-shadow-note">
          {{
            t("adminAuthorization.explainShadowed", {
              node: explained.shadowed_by.node,
              until: explained.shadowed_by.until,
            })
          }}
        </Alert>

        <Table v-if="explained.resolved.length" :label="t('adminAuthorization.explainTitle')">
          <TableHeader>
            <TableRow>
              <!-- `Granted by` is the point: a resolved set without provenance
                   says what a subject holds; the tier says which line to
                   edit. -->
              <TableHead>{{ t("adminAuthorization.verb") }}</TableHead>
              <TableHead>{{ t("adminAuthorization.grantedBy") }}</TableHead>
              <TableHead>{{ t("adminAuthorization.matchedSubject") }}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <TableRow v-for="(v, i) in explained.resolved" :key="i">
              <TableCell class="font-mono text-xs">{{ v.action }}</TableCell>
              <TableCell class="font-mono text-xs">{{ v.granted_by }}</TableCell>
              <TableCell class="font-mono text-xs">{{ v.subject }}</TableCell>
            </TableRow>
          </TableBody>
        </Table>
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
          <ul class="mt-1 list-disc pl-6">
            <li v-for="(layer, i) in explained.not_covered" :key="i">{{ layer }}</li>
          </ul>
        </details>
      </div>
    </section>

    <!-- ── Recent denials ───────────────────────────────────────────────── -->
    <section class="space-y-3" data-testid="panel-denials">
      <h2 class="font-mono text-xs uppercase tracking-[0.16em] text-muted-foreground">
        {{ t("adminAuthorization.denialsTitle") }}
      </h2>
      <AsyncState
        :loading="false"
        :error="denialsError"
        :empty="!denials.length"
        :empty-message="t('adminAuthorization.denialsNone')"
        :on-retry="loadDenials"
      >
        <Table :label="t('adminAuthorization.denialsTitle')">
          <TableHeader>
            <TableRow>
              <TableHead>{{ t("adminAuthorization.when") }}</TableHead>
              <TableHead>{{ t("adminAuthorization.subject") }}</TableHead>
              <TableHead>{{ t("adminAuthorization.coordinate") }}</TableHead>
              <TableHead>{{ t("adminAuthorization.reason") }}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <TableRow v-for="(d, i) in denials" :key="i">
              <TableCell class="text-xs">{{ d.timestamp }}</TableCell>
              <TableCell class="font-mono text-xs">{{ d.user_id ?? "-" }}</TableCell>
              <TableCell class="font-mono text-xs">{{ d.package_id ?? "-" }}</TableCell>
              <TableCell class="text-xs">{{ d.reason ?? "-" }}</TableCell>
            </TableRow>
          </TableBody>
        </Table>
      </AsyncState>
    </section>

    <!-- ── Retention ────────────────────────────────────────────────────────
         The fifth panel, and a link rather than a table: RFC 0016 already has a
         page that runs and renders the dry run, and duplicating it here would
         be a second place the report is read from. What §4.8 wants on *this*
         page is that the destructive direction is not out of sight, which the
         pointer serves. -->
    <section class="space-y-2" data-testid="panel-retention">
      <h2 class="font-mono text-xs uppercase tracking-[0.16em] text-muted-foreground">
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
