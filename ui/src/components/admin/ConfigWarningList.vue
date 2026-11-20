<script setup lang="ts">
/**
 * Config warnings: one bordered list, one row each, the same in every place the
 * config console lists them (active, staged, candidate).
 *
 * Each row leads with the warning's first sentence — the claim — and keeps the
 * explanation behind a disclosure. Six warnings were six paragraphs of equal
 * weight, each with its design-document citation inline, and the page read as
 * a wall before it read as a list. The citation is dropped from display
 * (`withoutDocRefs`); the stable `code` stays beside the key.
 */
import { useI18n } from "vue-i18n";
import { TriangleAlert } from "@lucide/vue";
import type { ConfigWarning } from "@/client/types.gen";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { leadSentence, withoutDocRefs } from "@/lib/plainCopy";

defineProps<{ warnings: ConfigWarning[]; dismissible?: boolean }>();
const emit = defineEmits<{ dismiss: [warning: ConfigWarning] }>();
const { t } = useI18n();

/** The server writes its messages lower-case, as log lines; a row leads with a capital. */
const capitalised = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);
const parts = (w: ConfigWarning) => leadSentence(capitalised(withoutDocRefs(w.message)));
</script>

<template>
  <!-- Opaque copper edge on the list: the one boundary that carries "this
       needs looking at", and rows divided by the soft rule inside it. -->
  <ul class="border border-copper divide-y divide-rule-soft">
    <li
      v-for="w in warnings"
      :key="`${w.code}@${w.path}`"
      class="flex items-start gap-3 px-4 py-3 text-sm"
      data-testid="config-warning"
    >
      <TriangleAlert class="mt-px h-4 w-4 shrink-0 text-copper" aria-hidden="true" />
      <!-- `min-w-0` so a long config path can shrink, and the code/badge pair
         wraps: a warning names a TOML key, and TOML keys are long enough to
         push the page sideways on a phone. -->
      <div class="min-w-0 flex-1 space-y-1">
        <p class="text-foreground">{{ parts(w).lead }}</p>
        <div class="flex flex-wrap items-center gap-2">
          <code class="font-mono text-xs text-muted-foreground break-all">{{ w.path }}</code>
          <Badge variant="outline">{{ w.code }}</Badge>
          <!-- Named in `[console].accepted`: read and kept by the operator. -->
          <span v-if="w.accepted" class="text-xs text-muted-foreground" data-testid="accepted">{{
            t("common.acceptedInConsole")
          }}</span>
        </div>
        <details v-if="parts(w).rest" class="group">
          <summary class="cursor-pointer text-xs text-muted-foreground hover:text-foreground">
            {{ t("configWarnings.howToFix") }}
          </summary>
          <p class="mt-1 text-muted-foreground">{{ parts(w).rest }}</p>
        </details>
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
    </li>
  </ul>
</template>
