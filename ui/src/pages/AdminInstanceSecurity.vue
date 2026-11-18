<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { RouterLink } from "vue-router";
import { instanceSecurity } from "@/client/sdk.gen";
import type { InstanceSecurity } from "@/client/types.gen";
import { PageHeader } from "@/components/ui/page-header";
import { AsyncState } from "@/components/ui/async-state";
import { Button } from "@/components/ui/button";
import StateMark, { type StatusState } from "@/components/admin/StateMark.vue";
import { docsUrl } from "@/config";
import { AUDIT_ACTION_LABEL_KEYS } from "@/config/codeLabels";
import { useAuth } from "@/composables/useAuth";

/**
 * The security of this server itself (RFC 0050 §4.2, §4.5): failed sign-ins,
 * bans, tokens seen from a new address, static tokens kept in clear, expiring
 * exemptions and the security warnings of the config. Each line names a
 * consequence and the page that acts on it. IPs and user ids appear only with
 * `audit:read`; the server leaves them out otherwise.
 */
const { t, locale } = useI18n();
const { can } = useAuth();

const WINDOWS = ["24h", "7d"] as const;
const WINDOW_LABEL: Record<(typeof WINDOWS)[number], string> = {
  "24h": "instanceSecurity.window24h",
  "7d": "instanceSecurity.window7d",
};
const windowSel = ref<(typeof WINDOWS)[number]>("24h");
const data = ref<InstanceSecurity | null>(null);
const loading = ref(false);
const error = ref<string | null>(null);

const sources = computed(() => data.value?.token_sources);

// A late answer for the window the viewer left must not overwrite the one
// they are looking at (as `SecurityTrends` does).
let latest = 0;
async function load() {
  const mine = ++latest;
  loading.value = true;
  error.value = null;
  try {
    const { data: body, error: err } = await instanceSecurity({
      query: { window: windowSel.value },
    });
    if (mine !== latest) return;
    if (err || !body) throw new Error(t("instanceSecurity.loadFailed"));
    data.value = body;
  } catch (e) {
    if (mine === latest) error.value = (e as Error).message;
  } finally {
    if (mine === latest) loading.value = false;
  }
}
watch(windowSel, load, { immediate: true });

const signInState = computed<StatusState>(() => {
  const s = data.value?.sign_ins;
  if (!s) return "unknown";
  if (s.state === "unknown") return "unknown";
  return s.rejected ? "attention" : "ok";
});
const byKind = computed(() =>
  Object.entries(data.value?.sign_ins.by_kind ?? {})
    .map(([k, n]) => `${AUDIT_ACTION_LABEL_KEYS[k] ? t(AUDIT_ACTION_LABEL_KEYS[k]!) : k} ${n}`)
    .join(", "),
);
const quiet = computed(() => {
  const d = data.value;
  if (!d) return false;
  return (
    d.sign_ins.state !== "unknown" &&
    !d.bans?.unknown_reason &&
    !d.exemptions?.unknown_reason &&
    !sources.value?.unknown_reason &&
    !d.sign_ins.rejected &&
    !(d.bans?.active ?? 0) &&
    !d.token_sources.new_source &&
    !d.static_tokens.plain &&
    !(d.exemptions?.expiring_7d ?? 0) &&
    !(d.warnings?.length ?? 0)
  );
});
</script>

<template>
  <div class="space-y-6">
    <PageHeader
      variant="display"
      :title="t('adminNav.instanceSecurity')"
      :description="t('instanceSecurity.description')"
    >
      <template #actions>
        <fieldset class="flex min-w-0">
          <legend class="sr-only">{{ t("instanceSecurity.window") }}</legend>
          <Button
            v-for="w in WINDOWS"
            :key="w"
            size="sm"
            :variant="windowSel === w ? 'default' : 'outline'"
            :aria-pressed="windowSel === w"
            @click="windowSel = w"
            >{{ t(WINDOW_LABEL[w]) }}</Button
          >
        </fieldset>
      </template>
    </PageHeader>

    <AsyncState :loading="loading && !data" :error="error" :on-retry="load">
      <template v-if="data">
        <p v-if="quiet" class="text-sm" data-testid="quiet">
          <StateMark state="ok" /> {{ t("instanceSecurity.quiet") }}
        </p>
        <ul class="divide-y divide-dashed divide-rule-soft border-y border-rule-soft text-sm">
          <li class="flex flex-col gap-1 py-2 lg:flex-row lg:gap-3" data-testid="line-sign-ins">
            <StateMark :state="signInState" />
            <span class="w-36 shrink-0 font-mono">{{ t("instanceSecurity.signIns") }}</span>
            <span class="min-w-0 flex-1">
              <template v-if="data.sign_ins.state === 'unknown'">{{
                t("securityStatus.couldNotRead")
              }}</template>
              <template v-else-if="data.sign_ins.rejected">
                <span v-if="data.sign_ins.partial" :title="t('instanceSecurity.atLeast')">≥ </span>
                {{
                  data.sign_ins.distinct_addresses != null
                    ? t("instanceSecurity.rejectedFrom", {
                        n: data.sign_ins.rejected,
                        addresses: data.sign_ins.distinct_addresses,
                      })
                    : t("instanceSecurity.rejected", { n: data.sign_ins.rejected })
                }}
                <span v-if="byKind" class="text-muted-foreground">({{ byKind }})</span>
                <span v-if="data.sign_ins.addresses?.length" class="block font-mono text-xs">
                  {{ data.sign_ins.addresses.map((a) => `${a.ip} ×${a.count}`).join(" · ") }}
                </span>
              </template>
              <template v-else>{{ t("instanceSecurity.noRejected") }}</template>
            </span>
          </li>
          <li
            v-if="data.bans"
            class="flex flex-col gap-1 py-2 lg:flex-row lg:gap-3"
            data-testid="line-bans"
          >
            <StateMark v-if="data.bans.unknown_reason" state="unknown" />
            <span v-else class="invisible" aria-hidden="true"><StateMark state="ok" /></span>
            <span class="w-36 shrink-0 font-mono">{{ t("instanceSecurity.bans") }}</span>
            <span class="min-w-0 flex-1">{{
              data.bans.unknown_reason
                ? t("securityStatus.couldNotRead")
                : t("instanceSecurity.bansLine", {
                    n: data.bans.active,
                    automatic: data.bans.automatic,
                  })
            }}</span>
            <RouterLink
              to="/admin/instance-security/blocks"
              class="text-primary underline underline-offset-[3px]"
              >→ {{ t("adminNav.blocks") }}</RouterLink
            >
          </li>
          <li class="flex flex-col gap-1 py-2 lg:flex-row lg:gap-3" data-testid="line-sources">
            <StateMark v-if="sources?.unknown_reason" state="unknown" />
            <span v-else class="invisible" aria-hidden="true"><StateMark state="ok" /></span>
            <span class="w-36 shrink-0 font-mono">{{ t("instanceSecurity.tokenSources") }}</span>
            <span v-if="sources?.unknown_reason" class="min-w-0 flex-1">{{
              t("securityStatus.couldNotRead")
            }}</span>
            <span v-else class="min-w-0 flex-1">
              <span v-if="sources?.partial" :title="t('instanceSecurity.atLeast')">≥ </span>
              {{ t("instanceSecurity.newSource", { n: data.token_sources.new_source }) }}
              <span
                v-for="s in data.token_sources.items ?? []"
                :key="`${s.user_id}|${s.ip}|${s.at}`"
                class="block font-mono text-xs"
                >{{ s.user_id }} · {{ s.ip }}</span
              >
            </span>
            <RouterLink
              v-if="can('audit:read')"
              to="/admin/system/audit-log"
              class="text-primary underline underline-offset-[3px]"
              >→ {{ t("adminNav.auditLog") }}</RouterLink
            >
          </li>
          <li class="flex flex-col gap-1 py-2 lg:flex-row lg:gap-3" data-testid="line-static">
            <StateMark :state="data.static_tokens.plain ? 'attention' : 'ok'" />
            <span class="w-36 shrink-0 font-mono">{{ t("instanceSecurity.staticTokens") }}</span>
            <span class="min-w-0 flex-1">
              <template v-if="data.static_tokens.plain">
                {{ t("instanceSecurity.plainTokens", { n: data.static_tokens.plain }) }}
                <span class="font-mono text-xs" data-testid="static-paths">{{
                  data.static_tokens.paths.join(", ")
                }}</span>
                <!-- With audit:read, whose tokens they are — beside the paths, not instead. -->
                <span
                  v-if="data.static_tokens.user_ids?.length"
                  class="block font-mono text-xs"
                  data-testid="static-users"
                  >{{ data.static_tokens.user_ids.join(", ") }}</span
                >
                <a
                  :href="docsUrl('guide/admin-access#static-tokens', locale)"
                  class="block text-primary underline underline-offset-[3px]"
                  >{{ t("instanceSecurity.hashThem") }}</a
                >
              </template>
              <template v-else>{{ t("instanceSecurity.noPlainTokens") }}</template>
            </span>
          </li>
          <li
            v-if="data.exemptions"
            class="flex flex-col gap-1 py-2 lg:flex-row lg:gap-3"
            data-testid="line-exemptions"
          >
            <StateMark v-if="data.exemptions.unknown_reason" state="unknown" />
            <span v-else class="invisible" aria-hidden="true"><StateMark state="ok" /></span>
            <span class="w-36 shrink-0 font-mono">{{ t("instanceSecurity.exemptions") }}</span>
            <span class="min-w-0 flex-1">{{
              data.exemptions.unknown_reason
                ? t("securityStatus.couldNotRead")
                : t("instanceSecurity.exemptionsLine", {
                    n: data.exemptions.expiring_7d,
                    self: data.exemptions.self_approved,
                  })
            }}</span>
            <RouterLink
              v-if="can('authz:read')"
              to="/admin/access/authorization"
              class="text-primary underline underline-offset-[3px]"
              >→ {{ t("adminNav.authorization") }}</RouterLink
            >
          </li>
          <li
            v-if="data.warnings"
            class="flex flex-col gap-1 py-2 lg:flex-row lg:gap-3"
            data-testid="line-warnings"
          >
            <StateMark :state="data.warnings.length ? 'attention' : 'ok'" />
            <span class="w-36 shrink-0 font-mono">{{ t("instanceSecurity.config") }}</span>
            <span class="min-w-0 flex-1">
              {{ t("instanceSecurity.warnings", { n: data.warnings.length }) }}
              <span class="font-mono text-xs">{{
                data.warnings.map((w) => w.code).join(", ")
              }}</span>
            </span>
            <RouterLink
              to="/admin/system/configuration"
              class="text-primary underline underline-offset-[3px]"
              >→ {{ t("adminNav.configReload") }}</RouterLink
            >
          </li>
        </ul>
      </template>
    </AsyncState>
  </div>
</template>
