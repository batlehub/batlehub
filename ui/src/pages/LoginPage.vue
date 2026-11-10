<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { ref, onMounted, computed } from "vue";
import { useRouter, useRoute } from "vue-router";
import { client } from "@/client/client.gen";
import { me, listOidcProviders } from "@/client/sdk.gen";
import type { MeResponse, OidcProviderInfo } from "@/client/types.gen";
import { useAuth, storeTokens } from "@/composables/useAuth";
import { generateOidcState } from "@/router";
import { API_BASE_URL } from "@/config";
import { Card, CardHeader, CardTitle, CardDescription, CardContent } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";

const { t, te } = useI18n();

const router = useRouter();
const route = useRoute();
const { isAuthenticated } = useAuth();

const inputToken = ref("");
/* Errors forwarded from the OIDC callback. The router sends a catalogue key
   (`loginPage.oidcStateMismatch`); the backend may forward its own sentence.
   `te` tells the two apart, so neither has to know about the other. */
function forwardedError(): string | null {
  const forwarded = route.query.error;
  if (typeof forwarded !== "string") return null;
  return te(forwarded) ? t(forwarded) : forwarded;
}

const error = ref<string | null>(forwardedError());
const loading = ref(false);
const oidcLoadingProvider = ref<string | null>(null);
const oidcProviders = ref<OidcProviderInfo[]>([]);

const redirect = computed(() =>
  typeof route.query.redirect === "string" ? route.query.redirect : "/packages",
);

// Already signed in → skip the login page.
onMounted(async () => {
  if (isAuthenticated.value) {
    router.replace(redirect.value);
    return;
  }
  // Fetch the list of available OIDC providers.
  try {
    const result = await listOidcProviders();
    oidcProviders.value = (result.data as OidcProviderInfo[] | undefined) ?? [];
  } catch {
    oidcProviders.value = [];
  }
});

async function submit() {
  const tok = inputToken.value.trim();
  if (!tok) {
    error.value = t("loginPage.enterAToken");
    return;
  }

  loading.value = true;
  error.value = null;

  // Probe the token before persisting it.
  client.setConfig({ auth: tok });
  try {
    const result = await me();
    const id = result.data as MeResponse | undefined;
    if (!id) {
      throw new Error("no data returned from server");
    }
    if (id.role === "anonymous") {
      error.value = t("loginPage.anonymousOnly");
      client.setConfig({ auth: undefined });
    } else {
      storeTokens(tok); // no refresh token or expiry for static tokens
      router.push(redirect.value);
    }
  } catch {
    error.value = t("loginPage.invalidToken");
    client.setConfig({ auth: undefined });
  } finally {
    loading.value = false;
  }
}

function signInWithOidc(providerName: string) {
  oidcLoadingProvider.value = providerName;
  // Generate a random state, store in sessionStorage for CSRF validation,
  // then hand off to the backend which threads it through to the provider.
  const state = generateOidcState();
  const params = new URLSearchParams({ state, provider: providerName });
  globalThis.location.href = `${API_BASE_URL}/api/v1/auth/oidc/login?${params}`;
}

function continueAnonymously() {
  const dest = redirect.value === "/login" ? "/packages" : redirect.value;
  router.push(dest);
}

function providerLabel(name: string): string {
  // Capitalise and replace dashes/underscores with spaces for display.
  return name.replace(/[-_]/g, " ").replace(/\b\w/g, (c) => c.toUpperCase());
}
</script>

<template>
  <div class="flex min-h-[calc(100vh-3.5rem)] items-center justify-center px-4">
    <Card class="w-full max-w-sm">
      <CardHeader class="space-y-1">
        <!-- The page's only heading, and its one Display element: this card
             *is* the page. `font-display` because DESIGN.md gives every view
             one step in the bitmap face, and an `h1` in the data face is what
             the ramp gate reads as a page with no display element at all. -->
        <CardTitle as="h1" class="font-display text-2xl font-bold tracking-[0.04em]">{{
          t("loginPage.signIn")
        }}</CardTitle>
        <CardDescription>{{ t("loginPage.authenticateToAccessProtected") }}</CardDescription>
      </CardHeader>

      <CardContent class="space-y-4">
        <!-- OIDC sign-in buttons (one per configured provider) -->
        <template v-if="oidcProviders.length > 0">
          <Button
            v-for="p in oidcProviders"
            :key="p.name"
            type="button"
            variant="outline"
            class="w-full"
            :disabled="oidcLoadingProvider !== null"
            @click="signInWithOidc(p.name)"
          >
            {{
              oidcLoadingProvider === p.name
                ? t("loginPage.redirecting")
                : oidcProviders.length === 1
                  ? t("loginPage.signInWithOidc")
                  : t("loginPage.signInWithProvider", { provider: providerLabel(p.name) })
            }}
          </Button>

          <!-- Divider between OIDC and token form -->
          <div class="relative">
            <div class="absolute inset-0 flex items-center">
              <span class="w-full border-t border-border/60" />
            </div>
            <div class="relative flex justify-center text-xs uppercase">
              <span class="bg-ground-raised px-2 font-mono text-muted-foreground tracking-widest">{{
                t("loginPage.orUseAToken")
              }}</span>
            </div>
          </div>
        </template>

        <!-- Static-token form -->
        <form class="space-y-4" @submit.prevent="submit">
          <div class="space-y-2">
            <Label for="token">{{ t("loginPage.bearerToken") }}</Label>
            <Input
              id="token"
              v-model="inputToken"
              type="password"
              :placeholder="t('loginPage.pasteYourTokenHere')"
              autocomplete="current-password"
            />
          </div>

          <p v-if="error" role="alert" class="text-sm text-destructive">
            {{ error }}
          </p>

          <Button type="submit" class="w-full" :disabled="loading">
            {{ loading ? t("loginPage.signingIn") : t("loginPage.signInWithToken") }}
          </Button>
        </form>

        <Button type="button" variant="ghost" class="w-full" @click="continueAnonymously">{{
          t("loginPage.continueWithoutSigningIn")
        }}</Button>
      </CardContent>
    </Card>
  </div>
</template>
