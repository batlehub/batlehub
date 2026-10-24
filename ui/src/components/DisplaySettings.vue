<script setup lang="ts">
/**
 * Theme and language, in one popover, as DESIGN draws every choice set: one
 * bounding box with 1px dividers and the chosen cell reversed to an ink block.
 *
 * They were two icon buttons that *cycled* — system → light → dark, system →
 * en → fr — so the current state lived only in an `aria-label`, and reaching
 * the one you wanted meant clicking through the others blind. Here all three
 * answers of each are on screen at once.
 *
 * Both stay stored *preferences* (RFC 0003 R12, §4.6): "System" is a real
 * answer, kept as such, so a later OS or browser change still reaches the
 * reader. Native radios, so the arrow keys move within a group and Tab moves
 * between them with no script of ours.
 */
import { ref } from "vue";
import { useI18n } from "vue-i18n";
import { useColorMode } from "@vueuse/core";
import { PopoverRoot, PopoverTrigger, PopoverPortal, PopoverContent } from "radix-vue";
import { SlidersHorizontal } from "@lucide/vue";
import { Button } from "@/components/ui/button";
import { readPreference, setLocalePreference, systemLocale, type LocalePreference } from "@/i18n";

const { t } = useI18n();

const mode = useColorMode({
  attribute: "data-theme",
  storageKey: "batlehub.theme",
  emitAuto: true,
});

/* Spelled out rather than built from the value, so the catalogue's reference
   gate can see every key (RFC 0004-bis §2.2). `auto` is vueuse's name for the
   stored "System". */
const THEMES = [
  { value: "auto", key: "theme.systemShort" },
  { value: "light", key: "theme.lightShort" },
  { value: "dark", key: "theme.darkShort" },
] as const;

const locale = ref<LocalePreference>(readPreference());
const LOCALES: { value: LocalePreference; key: string }[] = [
  { value: "system", key: "locale.system" },
  { value: "en", key: "locale.en" },
  { value: "fr", key: "locale.fr" },
];

function chooseLocale(value: LocalePreference): void {
  locale.value = value;
  setLocalePreference(value);
}

const CELL =
  "flex-1 cursor-pointer px-1 py-2 text-center font-mono text-xs uppercase tracking-wider text-muted-foreground hover:text-foreground has-[:checked]:bg-foreground has-[:checked]:text-background has-[:focus-visible]:outline has-[:focus-visible]:outline-2 has-[:focus-visible]:outline-offset-2 has-[:focus-visible]:outline-[var(--focus)]";
</script>

<template>
  <PopoverRoot>
    <PopoverTrigger as-child>
      <Button
        variant="ghost"
        size="icon"
        :title="t('displaySettings.title')"
        :aria-label="t('displaySettings.title')"
      >
        <SlidersHorizontal class="h-4 w-4" aria-hidden="true" />
      </Button>
    </PopoverTrigger>
    <PopoverPortal>
      <!-- DESIGN's popover: the sunk ground, a rule, no shadow, 264px. -->
      <PopoverContent
        align="end"
        :side-offset="6"
        class="z-50 w-[264px] space-y-4 border border-rule-strong bg-ground-sunk p-4 text-foreground"
      >
        <fieldset>
          <legend class="mb-2 font-mono text-xs uppercase tracking-wider text-muted-foreground">
            {{ t("displaySettings.theme") }}
          </legend>
          <div class="flex divide-x divide-border border border-border">
            <label v-for="opt in THEMES" :key="opt.value" :class="CELL">
              <input
                v-model="mode"
                type="radio"
                name="display-theme"
                :value="opt.value"
                class="sr-only"
              />
              {{ t(opt.key) }}
            </label>
          </div>
        </fieldset>
        <fieldset>
          <legend class="mb-2 font-mono text-xs uppercase tracking-wider text-muted-foreground">
            {{ t("locale.label") }}
          </legend>
          <div class="flex divide-x divide-border border border-border">
            <label v-for="opt in LOCALES" :key="opt.value" :class="CELL">
              <input
                type="radio"
                name="display-locale"
                :value="opt.value"
                :checked="locale === opt.value"
                class="sr-only"
                @change="chooseLocale(opt.value)"
              />
              {{ t(opt.key)
              }}<span v-if="opt.value === 'system'" class="sr-only">
                ({{ systemLocale().toUpperCase() }})</span
              >
            </label>
          </div>
        </fieldset>
      </PopoverContent>
    </PopoverPortal>
  </PopoverRoot>
</template>
