<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { computed } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { Menu, X, BookOpen, FolderKey } from "@lucide/vue";
import { useAuth } from "@/composables/useAuth";
import { primaryNav } from "@/config/navigation";
import { DOCS_URL } from "@/config";
import { Button } from "@/components/ui/button";
import ThemeToggle from "@/components/ThemeToggle.vue";
import LocaleToggle from "@/components/LocaleToggle.vue";
import AppNav from "./AppNav.vue";
import UserMenu from "./UserMenu.vue";
import GlobalBanner from "./GlobalBanner.vue";

const { t } = useI18n();

const mobileOpen = defineModel<boolean>("mobileOpen", { default: false });

const { identity, isAdmin, isAuthenticated, logout } = useAuth();
const route = useRoute();
const router = useRouter();

const isOidcUser = computed(() => isAuthenticated.value && !!identity.value?.auth_provider);

/* The bar is opaque and unblurred: DESIGN.md's Elevation rule leaves this
   system no blurs and no layered surfaces, and a translucent sticky bar is
   also how the nav labels were failing AA — measured at 1.3:1 against whatever
   happened to scroll under them. The hairline rule below separates the bar
   from the sheet; it does not need to be see-through to do that.

   The bar follows the viewer (RFC 0003 §4.1): an item appears when it can be
   used, so nothing here leads to a redirect. Diagnostics moved to /tools —
   they are linked from the errors that motivate them instead of holding
   primary-nav weight for the rest of the year.

   All of them go through one segment box, Admin included. Splitting Admin out
   behind a divider gave the same choice two different shapes and made the bar
   read as two bars; the proof's masthead runs Packages/Setup/Admin as three
   cells of one control, and Admin's copper ink is what marks it out (AppNav). */
const navLinks = computed(() =>
  primaryNav({
    isAuthenticated: isAuthenticated.value,
    isAdmin: isAdmin.value,
    hasRegistryAccess: identity.value?.has_registry_access !== false,
  }),
);

function isActive(to: string) {
  return route.path === to || route.path.startsWith(to + "/");
}

function handleLogout() {
  logout();
  router.push("/login");
  mobileOpen.value = false;
}
</script>

<template>
  <header class="sticky top-0 z-40 border-b border-rule-soft bg-ground-sunk">
    <!-- The instance's announcement, above the masthead rather than under it.

         It sat below the nav, which put a maintenance or outage notice *third*
         in the reading order — after the wordmark and the whole primary bar —
         for something whose entire job is to be read before anything else on
         the page. It stays inside the sticky header on purpose: an error-level
         banner that scrolls away is a banner an operator can miss by scrolling,
         and moving it out to `App.vue` would have bought "topmost" at the cost
         of "always visible". Here it is both. -->
    <GlobalBanner />

    <!-- Full-bleed, not a centred container. The proof's `.masthead` is a body
         child with `max-width:100%` and `padding: --s3 --s5`, so the wordmark
         is flush to the left edge and the controls to the right: a bar that
         spans the window edge to edge but whose *contents* stop 260px short of
         it on a wide screen is drawing a frame it does not fill.

         **The bar reorganises at `lg`, and it used to be `md`.**
         The proof collapses its masthead at 900 (`@media (max-width:900px)`),
         between our `md` and `lg`, and this component took `md` — 132px early.
         Measured with a session, on any page: the desktop bar's min-content is
         808px in English (wordmark 181 · nav 227 · controls 304) and **900px in
         French**, where the same three destinations read `PAQUETS ·
         CONFIGURATION · ADMINISTRATION` (362px). So every width from 768 to 899
         made the *document* scroll sideways — the one thing DESIGN.md's
         Own-Container Overflow Rule forbids it to do — and the dotted spacer,
         which is the bar's designed slack, was already collapsed to 0.
         `lg` is the first Tailwind step that fits the widest locale, and it
         collapses to the hamburger masthead this component already ships rather
         than putting primary wayfinding behind a scroll.
         A locale is not a layout variant: the breakpoint has to fit the widest
         one, not the one the developer happens to be reading in.
         Edge padding still steps at `md` on purpose — `GlobalBanner` and
         `AppFooter` both key `px-4 md:px-6` to "the bars it sits with", so
         moving this one alone would misalign the three vertical edges between
         768 and 1023. Only the *reorganisation* moved. -->
    <div class="flex h-14 items-center gap-3 px-4 md:px-6 lg:gap-6">
      <!-- The wordmark goes home. It pointed at /packages from when `/` was a
           blind redirect there; Phase 4 made `/` a real identity-aware surface
           (RFC 0003 §4.3), so the logo had been skipping past it ever since.

           The mark is the brand mark itself, drawn on the same pixel grid as
           the display face rather than being a stock parcel icon from another
           drawing system. It inherits the link's colour (`currentColor`), which
           is what keeps it right in both renditions — `public/logo.svg` is the
           standalone twin, and it has to carry its own colour because a favicon
           inherits nothing. Ink not crimson: the accent is one dot, spent where
           it terminates the word, so the bar holds a single saturated pixel
           instead of a saturated line of type. -->
      <RouterLink
        to="/"
        class="flex items-center gap-3 shrink-0 font-display text-sub leading-none tracking-[0.04em] text-foreground transition-colors hover:text-copper sm:text-xl"
      >
        <svg viewBox="0 0 26 41" aria-hidden="true" fill="currentColor" class="h-6 w-auto shrink-0">
          <path
            d="M3 0h2v1h-2zM6 0h2v1h-2zM18 0h2v1h-2zM21 0h2v1h-2zM3 1h2v1h-2zM6 1h2v1h-2zM18 1h2v1h-2zM21 1h2v1h-2zM3 2h5v1h-5zM18 2h5v1h-5zM3 3h5v1h-5zM10 3h2v1h-2zM14 3h2v1h-2zM18 3h5v1h-5zM4 4h4v1h-4zM10 4h2v1h-2zM14 4h2v1h-2zM18 4h4v1h-4zM5 5h2v1h-2zM11 5h1v1h-1zM14 5h1v1h-1zM19 5h2v1h-2zM6 6h14v1h-14zM5 7h16v1h-16zM5 8h7v1h-7zM14 8h7v1h-7zM5 9h16v1h-16zM6 10h14v1h-14zM7 11h12v1h-12zM5 12h1v1h-1zM8 12h1v1h-1zM17 12h1v1h-1zM20 12h1v1h-1zM0 13h26v1h-26zM0 14h26v1h-26zM0 15h2v1h-2zM24 15h2v1h-2zM0 16h2v1h-2zM8 16h4v1h-4zM24 16h2v1h-2zM0 17h2v1h-2zM4 17h3v1h-3zM8 17h4v1h-4zM24 17h2v1h-2zM0 18h2v1h-2zM4 18h3v1h-3zM8 18h4v1h-4zM13 18h3v1h-3zM17 18h4v1h-4zM24 18h2v1h-2zM0 19h2v1h-2zM4 19h3v1h-3zM8 19h1v1h-1zM11 19h1v1h-1zM13 19h3v1h-3zM17 19h4v1h-4zM24 19h2v1h-2zM0 20h2v1h-2zM4 20h3v1h-3zM8 20h1v1h-1zM11 20h1v1h-1zM13 20h1v1h-1zM15 20h1v1h-1zM18 20h4v1h-4zM24 20h2v1h-2zM0 21h2v1h-2zM4 21h3v1h-3zM8 21h4v1h-4zM13 21h1v1h-1zM15 21h1v1h-1zM18 21h4v1h-4zM24 21h2v1h-2zM0 22h2v1h-2zM4 22h3v1h-3zM8 22h4v1h-4zM13 22h3v1h-3zM19 22h4v1h-4zM24 22h2v1h-2zM0 23h2v1h-2zM4 23h3v1h-3zM8 23h4v1h-4zM13 23h3v1h-3zM19 23h4v1h-4zM24 23h2v1h-2zM0 24h2v1h-2zM4 24h3v1h-3zM8 24h4v1h-4zM13 24h3v1h-3zM20 24h6v1h-6zM0 25h2v1h-2zM4 25h3v1h-3zM8 25h4v1h-4zM13 25h3v1h-3zM20 25h6v1h-6zM0 26h26v1h-26zM0 27h26v1h-26zM0 28h2v1h-2zM24 28h2v1h-2zM0 29h2v1h-2zM9 29h3v1h-3zM24 29h2v1h-2zM0 30h2v1h-2zM4 30h4v1h-4zM9 30h3v1h-3zM18 30h3v1h-3zM24 30h2v1h-2zM0 31h2v1h-2zM4 31h4v1h-4zM9 31h3v1h-3zM13 31h4v1h-4zM18 31h3v1h-3zM24 31h2v1h-2zM0 32h2v1h-2zM4 32h4v1h-4zM9 32h3v1h-3zM13 32h4v1h-4zM18 32h3v1h-3zM22 32h1v1h-1zM24 32h2v1h-2zM0 33h2v1h-2zM4 33h1v1h-1zM7 33h1v1h-1zM9 33h3v1h-3zM13 33h4v1h-4zM18 33h3v1h-3zM22 33h1v1h-1zM24 33h2v1h-2zM0 34h2v1h-2zM4 34h1v1h-1zM7 34h1v1h-1zM9 34h3v1h-3zM13 34h1v1h-1zM16 34h1v1h-1zM18 34h3v1h-3zM22 34h1v1h-1zM24 34h2v1h-2zM0 35h2v1h-2zM4 35h4v1h-4zM9 35h3v1h-3zM13 35h1v1h-1zM16 35h1v1h-1zM18 35h3v1h-3zM22 35h1v1h-1zM24 35h2v1h-2zM0 36h2v1h-2zM4 36h4v1h-4zM9 36h3v1h-3zM13 36h4v1h-4zM18 36h3v1h-3zM22 36h1v1h-1zM24 36h2v1h-2zM0 37h2v1h-2zM4 37h4v1h-4zM9 37h3v1h-3zM13 37h4v1h-4zM18 37h3v1h-3zM22 37h1v1h-1zM24 37h2v1h-2zM0 38h2v1h-2zM4 38h4v1h-4zM9 38h3v1h-3zM13 38h4v1h-4zM18 38h3v1h-3zM22 38h1v1h-1zM24 38h2v1h-2zM0 39h26v1h-26zM0 40h26v1h-26z"
          />
        </svg>
        <span>BatleHub<span class="text-primary">.</span></span>
      </RouterLink>

      <AppNav :links="navLinks" variant="desktop" />

      <!-- The proof's textured spacer: dot-matrix as a *material*, at the same
           9px pitch as the board's texture swatch, living in the one box of the
           bar that holds no text and so cannot lose contrast to it. It is also
           the flex spacer, which means it collapses to nothing exactly when the
           bar gets tight — the texture is never what survives a narrow
           viewport. Masked to fade at both edges so it reads as a field the bar
           passes through rather than a panel with borders. -->
      <div class="relative hidden self-stretch lg:block lg:flex-1" aria-hidden="true">
        <span
          class="pointer-events-none absolute inset-0 opacity-55 [background-image:radial-gradient(var(--rule-soft)_1px,transparent_1.5px)] [background-size:9px_9px] [mask-image:linear-gradient(90deg,transparent,#000_40%,transparent)]"
        />
      </div>

      <!-- Right side -->
      <div class="ml-auto flex items-center gap-2 lg:ml-0">
        <!-- The proof's `.ctl`: a bordered uppercase cell, the same edge weight
             as the nav segment, so a control and a destination are told apart
             by shape rather than by two unrelated shapes both being blue-ish.

             The word is spent from `lg`, the icon carries it below that, and it
             is `lg` itself that needs this rather than the narrow widths: at
             exactly 1024 the French bar wants 1061px with the label and 1009
             without it, so the first width that shows the segment run would have
             scrolled sideways in French on the day it stopped doing so in
             English. Collapsing the bar at `lg` fixes 768–899 (see the
             breakpoint note above); this is what makes the step *into* the
             desktop layout fit too.

             This label, and not another 50px: the nav is the page's primary
             wayfinding, the wordmark is identity, and the user cell is already
             initials-only until `lg`. A convenience link to external docs is the
             one control here that reads the same as an icon — which is how the
             two toggles beside it already work — and `aria-label` keeps its name
             at every width, so nothing is lost but the glyphs. -->
        <a
          :href="DOCS_URL"
          target="_blank"
          rel="noopener noreferrer"
          :aria-label="t('common.docs')"
          class="hidden sm:flex items-center gap-2 whitespace-nowrap border border-border px-3 py-2 text-xs uppercase tracking-[0.1em] text-muted-foreground transition-colors hover:border-muted-foreground hover:text-foreground"
        >
          <BookOpen class="h-3.5 w-3.5" />
          <span class="hidden lg:inline">{{ t("common.docs") }}</span>
        </a>
        <LocaleToggle />
        <ThemeToggle />

        <UserMenu />

        <!-- Mobile menu toggle -->
        <Button
          variant="ghost"
          size="icon"
          class="lg:hidden"
          :aria-label="mobileOpen ? t('a11y.closeMenu') : t('a11y.openMenu')"
          :aria-expanded="mobileOpen"
          aria-controls="mobile-nav"
          @click="mobileOpen = !mobileOpen"
        >
          <X v-if="mobileOpen" class="h-4 w-4" />
          <Menu v-else class="h-4 w-4" />
        </Button>
      </div>
    </div>

    <!-- Mobile nav.

         One ruled run, not a stack of floating pills: the segment grammar the
         desktop bar uses, turned on its side. Every row is a cell of the same
         box and the chosen one reverses to ink, so "where am I" is answered the
         same way at both widths. -->
    <div
      v-if="mobileOpen"
      id="mobile-nav"
      class="lg:hidden border-t border-rule-soft bg-ground-sunk px-4 py-4"
    >
      <nav :aria-label="t('nav.aria')" class="border border-border divide-y divide-border">
        <AppNav :links="navLinks" variant="mobile" @navigate="mobileOpen = false" />
        <RouterLink
          v-if="isOidcUser"
          to="/me/tokens"
          :aria-current="isActive('/me/tokens') ? 'page' : undefined"
          :class="[
            'block px-3 py-2.5 text-xs uppercase tracking-[0.1em] transition-colors',
            isActive('/me/tokens')
              ? 'bg-foreground text-background font-bold'
              : 'text-muted-foreground hover:text-foreground',
          ]"
          @click="mobileOpen = false"
          >{{ t("appHeader.myTokens") }}</RouterLink
        >
        <RouterLink
          v-if="isAuthenticated"
          to="/me/profile"
          :aria-current="isActive('/me/profile') ? 'page' : undefined"
          :class="[
            'block px-3 py-2.5 text-xs uppercase tracking-[0.1em] transition-colors',
            isActive('/me/profile')
              ? 'bg-foreground text-background font-bold'
              : 'text-muted-foreground hover:text-foreground',
          ]"
          @click="mobileOpen = false"
          >{{ t("appHeader.myProfile") }}</RouterLink
        >
        <RouterLink
          v-if="isAuthenticated"
          to="/me/namespace"
          :aria-current="isActive('/me/namespace') ? 'page' : undefined"
          :class="[
            'flex items-center gap-2 px-3 py-2.5 text-xs uppercase tracking-[0.1em] transition-colors',
            isActive('/me/namespace')
              ? 'bg-foreground text-background font-bold'
              : 'text-muted-foreground hover:text-foreground',
          ]"
          @click="mobileOpen = false"
        >
          <FolderKey class="h-3.5 w-3.5" />
          {{ t("appHeader.myNamespace") }}
        </RouterLink>
        <a
          :href="DOCS_URL"
          target="_blank"
          rel="noopener noreferrer"
          class="flex items-center gap-2 px-3 py-2.5 text-xs uppercase tracking-[0.1em] text-muted-foreground hover:text-foreground transition-colors"
          @click="mobileOpen = false"
        >
          <BookOpen class="h-3.5 w-3.5" />
          {{ t("common.documentation") }}
        </a>
      </nav>
      <button
        v-if="isAuthenticated"
        type="button"
        class="mt-3 block w-full border border-border px-3 py-2.5 text-left text-xs uppercase tracking-[0.1em] text-destructive transition-colors hover:border-destructive"
        @click="handleLogout"
      >
        {{ t("appHeader.signOut") }}
      </button>
    </div>
  </header>
</template>
