<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { onMounted, ref } from "vue";
import { useRoute } from "vue-router";

import { adminAuthzExplain } from "@/client/sdk.gen";
import type { ExplainResponse } from "@/client/types.gen";
import { extractMessage } from "@/composables/useApi";
import CoordinateFields from "@/components/admin/CoordinateFields.vue";
import { Select } from "@/components/ui/select";
import { Combobox } from "@/components/ui/combobox";
import { useRegistryOptions } from "@/composables/useRegistryOptions";
import { Button } from "@/components/ui/button";
import { Announcer } from "@/components/ui/announcer";
import { Alert } from "@/components/ui/alert";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "@/components/ui/table";

/**
 * "Why was this refused?" for one subject, verb and coordinate. The registry is
 * the page's — the Exemptions panel lists that registry's — so it comes down as
 * a model; the rest of the question, and its answer, live here. The whole
 * question can arrive in the query string.
 */
const registry = defineModel<string>("registry", { required: true });

const { t } = useI18n();
const route = useRoute();
const { options: registryOptions } = useRegistryOptions();

const subject = ref("role:user");
const action = ref("releases:read");
const packageName = ref("");
const version = ref("");

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

onMounted(() => {
  const one = (v: unknown) => (typeof v === "string" ? v : "");
  registry.value = one(route.query.registry) || registry.value;
  packageName.value = one(route.query.package) || packageName.value;
  version.value = one(route.query.version) || version.value;
  if (one(route.query.subject)) subject.value = one(route.query.subject);
  if (one(route.query.action)) action.value = one(route.query.action);
});
</script>

<template>
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
        <CoordinateFields
          v-model:name="packageName"
          v-model:version="version"
          id-prefix="az"
          :registry="registry"
        />
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
</template>
