<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { onMounted, ref } from "vue";
import { API_BASE_URL, REPORT_BUG_URL, REPORT_SECURITY_URL } from "@/config";

const { t } = useI18n();

const appVersion = ref<string | null>(null);

onMounted(async () => {
  try {
    const res = await fetch(`${API_BASE_URL}/healthz`);
    if (res.ok) {
      const data = (await res.json()) as { version?: string };
      appVersion.value = data.version ?? null;
    }
  } catch {
    // Best-effort only — the footer just omits the version if unreachable.
  }
});
</script>

<template>
  <footer class="border-t border-rule-soft mt-10">
    <!-- The masthead's twin: both are ruled bars framing the sheet, so they
         take the same edge. Left centred while the masthead went full-bleed,
         the two disagreed by ~260px at 1920 — the frame's top rail flush to the
         window and its bottom rail inset. The hairline is `--rule-soft`, the
         separator token, rather than `--border` (`--rule-strong`) at an
         opacity: the same substitution the masthead's rule was making.

         The comment sits inside the element on purpose: a template whose first
         node is a comment makes the component multi-root, and `$el` is then a
         fragment anchor rather than the <footer> — which is exactly how the
         contentinfo-landmark test caught this. -->
    <div
      class="flex flex-wrap items-center justify-between gap-2 px-4 py-3 text-xs text-muted-foreground md:px-6"
    >
      <span v-if="appVersion">{{ t("appFooter.version", { version: appVersion }) }}</span>
      <span v-else />
      <div class="flex items-center gap-3">
        <a
          :href="REPORT_BUG_URL"
          target="_blank"
          rel="noopener noreferrer"
          class="hover:text-accent-foreground transition-colors"
          >{{ t("appFooter.reportABug") }}</a
        >
        <a
          :href="REPORT_SECURITY_URL"
          target="_blank"
          rel="noopener noreferrer"
          class="hover:text-accent-foreground transition-colors"
          >{{ t("appFooter.reportASecurityIssue") }}</a
        >
      </div>
    </div>
  </footer>
</template>
