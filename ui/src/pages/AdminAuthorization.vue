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
 * is actively serving requests the model refuses. Then Exemptions, then Explain and the download simulation.
 * Recent denials sits below the fold because it is diagnostic rather than
 * standing risk — an operator arrives at it from a ticket, not from a review.
 */
import { useI18n } from "vue-i18n";
import { ref } from "vue";
import { RouterLink } from "vue-router";

import { PageHeader } from "@/components/ui/page-header";
import AuthzShadowPanel from "@/components/admin/authorization/AuthzShadowPanel.vue";
import AuthzExemptionsPanel from "@/components/admin/authorization/AuthzExemptionsPanel.vue";
import AuthzExplainPanel from "@/components/admin/authorization/AuthzExplainPanel.vue";
import AuthzDenialsPanel from "@/components/admin/authorization/AuthzDenialsPanel.vue";
import AuthzSimulatePanel from "@/components/admin/authorization/AuthzSimulatePanel.vue";
import { useAuth } from "@/composables/useAuth";

const { t } = useI18n();
const { can } = useAuth();

// The three standing-risk panels load without being asked. An operator who
// has to press a button to discover a shadow-mode node is serving everything has a page
// that does not do its job. Each panel loads on its own mount; Exemptions
// follows `registry`, which a `?registry=` query sets as Explain mounts.

/** The registry the Explain form names, and the one whose exemptions are listed. */
const registry = ref("");
</script>

<template>
  <div class="space-y-6">
    <PageHeader
      variant="display"
      :title="t('adminAuthorization.title')"
      :description="t('adminAuthorization.description')"
    />

    <!-- ── Shadow ──────────────────────────────────────────────────────────
         First, and the only panel that can be *currently wrong*: a shadowed
         node serves what the model refuses. -->
    <AuthzShadowPanel />

    <!-- ── Exemptions ───────────────────────────────────────────────────── -->
    <AuthzExemptionsPanel :registry="registry" />

    <!-- ── Explain ──────────────────────────────────────────────────────── -->
    <!-- RFC 0050 §6.7: grants for one coordinate (`#explain`). -->
    <div id="explain" class="scroll-mt-16">
      <AuthzExplainPanel v-model:registry="registry" />
    </div>

    <!-- ── Simulate ─────────────────────────────────────────────────────────
         The whole download (blocks, then rules), where Explain resolves grants
         only. The retired access check lands here (`#simulate`). -->
    <div v-if="can('authz:read')" id="simulate" class="scroll-mt-16">
      <AuthzSimulatePanel />
    </div>

    <!-- ── Recent denials ───────────────────────────────────────────────── -->
    <AuthzDenialsPanel />

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
