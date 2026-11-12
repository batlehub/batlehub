<script setup lang="ts">
/**
 * Config warnings, one copper box each, the same in every place the config
 * console lists them (active, staged, candidate): they had three paddings.
 */
import { useI18n } from "vue-i18n";
import type { ConfigWarning } from "@/client/types.gen";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";

defineProps<{ warnings: ConfigWarning[]; dismissible?: boolean }>();
const emit = defineEmits<{ dismiss: [warning: ConfigWarning] }>();
const { t } = useI18n();
</script>

<template>
  <!-- Opaque copper edge: alpha on the line that carries the contrast was the
       one thing the other components had stopped doing. -->
  <div role="list" class="space-y-2">
    <div
      v-for="w in warnings"
      :key="`${w.code}@${w.path}`"
      role="listitem"
      class="flex items-start gap-3 border border-copper px-4 py-2 text-sm text-copper"
    >
      <!-- `min-w-0` so a long config path can shrink, and the code/badge pair
         wraps: a warning names a TOML key, and TOML keys are long enough to
         push the page sideways on a phone. -->
      <div class="min-w-0 flex-1 space-y-1">
        <div class="flex flex-wrap items-center gap-2">
          <code class="font-mono text-xs break-all">{{ w.path }}</code>
          <Badge variant="outline">{{ w.code }}</Badge>
        </div>
        <p>{{ w.message }}</p>
      </div>
      <!-- A warning that must stay in view while it applies (RFC 0045
         `mise.overlay.forced-shadow`) has no button; an older server sends no
         field, which is the old behaviour. -->
      <Button
        v-if="dismissible && w.dismissible !== false"
        variant="ghost"
        size="sm"
        @click="emit('dismiss', w)"
        >{{ t("common.dismiss") }}</Button
      >
    </div>
  </div>
</template>
