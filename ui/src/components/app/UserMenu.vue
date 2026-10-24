<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { computed } from "vue";
import { RouterLink, useRouter } from "vue-router";
import { User, KeyRound, FolderKey, Terminal, Wrench, LogOut, ChevronDown } from "@lucide/vue";
import {
  DropdownMenuRoot,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuLabel,
} from "radix-vue";
import { useAuth } from "@/composables/useAuth";
import { Badge } from "@/components/ui/badge";

import { roleLabel } from "@/lib/roles";
const { t } = useI18n();

const { identity, isAuthenticated, logout } = useAuth();
const router = useRouter();

const isOidcUser = computed(() => isAuthenticated.value && !!identity.value?.auth_provider);

const userInitials = computed(() => {
  const uid = identity.value?.user_id;
  if (!uid) return "?";
  return uid.slice(0, 2).toUpperCase();
});

function handleLogout() {
  logout();
  router.push("/login");
}
</script>

<template>
  <template v-if="isAuthenticated">
    <DropdownMenuRoot>
      <!-- The bar's grammar (AppNav, the design proof's `.masthead`): a ruled
           cell, meta type, state on ink and rule weight. The tinted hover it
           replaces rode on `bg-accent`, which resolves to `--ground-raised` at
           1.06:1 — a fill nobody can see is not a hover state. -->
      <DropdownMenuTrigger
        class="hidden sm:flex items-center gap-2 border border-border px-2 py-1 text-xs text-muted-foreground transition-colors outline-none hover:border-muted-foreground hover:text-foreground"
      >
        <div
          class="h-6 w-6 bg-foreground text-background flex items-center justify-center text-xs font-bold shrink-0"
        >
          {{ userInitials }}
        </div>
        <span class="hidden lg:inline max-w-[10rem] truncate tracking-[0.1em]">
          {{ identity?.user_id }}
        </span>
        <!-- One neutral badge, named by the catalogue. Admin was a hardcoded
             English "admin" in copper — and copper means "held", not "you". -->
        <Badge v-if="identity?.role !== 'anonymous'" variant="outline" class="text-xs">
          {{ roleLabel(t, identity?.role) }}
        </Badge>
        <ChevronDown class="h-3.5 w-3.5 text-muted-foreground shrink-0" />
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="end"
        class="z-50 min-w-[11rem] rounded-sm border border-border bg-popover p-1"
      >
        <DropdownMenuLabel
          class="px-2 py-1.5 text-xs text-muted-foreground font-mono font-normal truncate"
        >
          {{ identity?.user_id }}
        </DropdownMenuLabel>
        <DropdownMenuSeparator class="my-1 h-px bg-border" />
        <DropdownMenuItem as-child>
          <RouterLink
            to="/me/profile"
            class="flex items-center gap-2 px-2 py-1.5 text-sm rounded-sm cursor-pointer hover:bg-accent hover:text-accent-foreground outline-none transition-colors"
          >
            <User class="h-3.5 w-3.5" />
            {{ t("userMenu.myProfile") }}
          </RouterLink>
        </DropdownMenuItem>
        <DropdownMenuItem v-if="isOidcUser" as-child>
          <RouterLink
            to="/me/tokens"
            class="flex items-center gap-2 px-2 py-1.5 text-sm rounded-sm cursor-pointer hover:bg-accent hover:text-accent-foreground outline-none transition-colors"
          >
            <KeyRound class="h-3.5 w-3.5" />
            {{ t("userMenu.myTokens") }}
          </RouterLink>
        </DropdownMenuItem>
        <DropdownMenuItem as-child>
          <RouterLink
            to="/me/namespace"
            class="flex items-center gap-2 px-2 py-1.5 text-sm rounded-sm cursor-pointer hover:bg-accent hover:text-accent-foreground outline-none transition-colors"
          >
            <FolderKey class="h-3.5 w-3.5" />
            {{ t("userMenu.myNamespace") }}
          </RouterLink>
        </DropdownMenuItem>
        <DropdownMenuItem as-child>
          <RouterLink
            to="/me/cli"
            class="flex items-center gap-2 px-2 py-1.5 text-sm rounded-sm cursor-pointer hover:bg-accent hover:text-accent-foreground outline-none transition-colors"
          >
            <Terminal class="h-3.5 w-3.5" />
            {{ t("userMenu.downloadCli") }}
          </RouterLink>
        </DropdownMenuItem>
        <DropdownMenuSeparator class="my-1 h-px bg-border" />
        <!-- RFC 0003 demoted diagnostics out of the primary bar on the grounds
             that they are "reachable from the user menu" (`navigation.ts`).
             They were not: the entry was never written, so `/tools` was
             unreachable except by URL or from `PackageDetailPage`'s denial
             link. Demoted, not hidden — this is the half that was owed.

             Points at the first tab rather than `/tools`, which is a
             `SECTION_INDEXES` key: nothing in a menu should lead to a
             redirect. Labelled from `tools.title`, the hub's own name, so the
             place is not called two different things. -->
        <DropdownMenuItem as-child>
          <RouterLink
            to="/tools/access-check"
            class="flex items-center gap-2 px-2 py-1.5 text-sm rounded-sm cursor-pointer hover:bg-accent hover:text-accent-foreground outline-none transition-colors"
          >
            <Wrench class="h-3.5 w-3.5" />
            {{ t("tools.title") }}
          </RouterLink>
        </DropdownMenuItem>
        <DropdownMenuSeparator class="my-1 h-px bg-border" />
        <DropdownMenuItem
          class="flex items-center gap-2 px-2 py-1.5 text-sm rounded-sm cursor-pointer hover:bg-accent text-destructive hover:text-destructive outline-none transition-colors"
          @select="handleLogout"
        >
          <LogOut class="h-3.5 w-3.5" />
          {{ t("userMenu.signOut") }}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenuRoot>
  </template>
  <!-- The one action an anonymous viewer has, cut as the bar's `.ctl` so it
       reads as a control beside Docs rather than as loose prose in a ruled bar. -->
  <RouterLink
    v-else
    to="/login"
    class="shrink-0 whitespace-nowrap border border-border px-3 py-2 text-xs uppercase tracking-[0.1em] text-muted-foreground transition-colors hover:border-muted-foreground hover:text-foreground"
    >{{ t("userMenu.signIn") }}</RouterLink
  >
</template>
