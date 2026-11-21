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
/* Errors forwarded from the OIDC callback. Only a `loginPage.*` catalogue key
   is shown as such: anything else in `?error=` is attacker-writable text in
   this page's own alert box, so it reads as the generic failure instead. */
function forwardedError(): string | null {
  const forwarded = route.query.error;
  if (typeof forwarded !== "string") return null;
  return forwarded.startsWith("loginPage.") && te(forwarded)
    ? t(forwarded)
    : t("loginPage.oidcLoginFailed");
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
    void router.replace(redirect.value);
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
      // The client answers `{ error, response }` instead of throwing, and
      // `response` is absent when nothing came back at all: offline, DNS, TLS,
      // or a CORS refusal from a server that does not list this page's origin.
      // Every one of those was reported as a bad token, which sent an operator
      // on a dev front to check a token that was fine.
      const status = result.response?.status;
      error.value =
        status === undefined
          ? unreachable()
          : status === 401 || status === 403
            ? t("loginPage.invalidToken")
            : t("loginPage.serverError", { status });
      client.setConfig({ auth: undefined });
      return;
    }
    // An unknown token is answered as an anonymous visitor (and logged as a
    // rejected sign-in), so this is a token the server does not recognise.
    if (id.role === "anonymous") {
      error.value = t("loginPage.anonymousOnly");
      client.setConfig({ auth: undefined });
    } else {
      storeTokens(tok); // no refresh token or expiry for static tokens
      void router.push(redirect.value);
    }
  } catch {
    // Only a fetch that never got an answer rejects.
    error.value = unreachable();
    client.setConfig({ auth: undefined });
  } finally {
    loading.value = false;
  }
}

function unreachable(): string {
  return t("loginPage.serverUnreachable", {
    server: API_BASE_URL || globalThis.location.origin,
    origin: globalThis.location.origin,
  });
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
  void router.push(dest);
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
             *is* the page. Titles are set in the interface face
             (DESIGN.md, The Interface Face Rule); the bitmap face is kept for
             the wordmark and the home greeting. -->
        <CardTitle as="h1" class="text-2xl font-semibold tracking-tight">{{
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
