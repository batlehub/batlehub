import { ref, watchEffect, type Ref } from "vue";

import { i18n } from "@/i18n";

/**
 * RFC 0003 §13 recorded these two as deliberately *not* catalogue keys, on the
 * grounds that a misrouted API is a deployment fault an operator reads. RFC
 * 0004-bis §4.1 reverses that: the operator reading it is as likely to be
 * French as the user reading anything else, and an exemption the gate cannot
 * see is indistinguishable from a string nobody noticed. The gate runs at
 * `--max 0`, so an exemption has to be declared rather than argued in a comment.
 *
 * `i18n.global.t` rather than `useI18n()`: this file is a composable *and* a
 * module-scope helper, and `extractMessage` is called from outside `setup`.
 */
const t = (key: string) => i18n.global.t(key);

export function extractMessage(err: unknown): string {
  if (err == null) return t("common.unknownError");
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  if (typeof err === "object") {
    const e = err as Record<string, unknown>;
    if (typeof e["message"] === "string") return e["message"];
    if (typeof e["error"] === "string") return e["error"];
    // `String(err)` would be "[object Object]", which tells the reader nothing.
    return t("common.unknownError");
  }
  return String(err);
}

interface ApiState<T> {
  data: Ref<T | null>;
  error: Ref<string | null>;
  loading: Ref<boolean>;
  reload: () => void;
}

export function useApi<T>(
  fn: () => Promise<{ data?: unknown; error?: unknown }>,
  deps: Ref<unknown>[] = [],
): ApiState<T> {
  const data = ref<T | null>(null) as Ref<T | null>;
  const error = ref<string | null>(null);
  const loading = ref(false);
  const tick = ref(0);

  /**
   * Which call owns the state.
   *
   * Nothing here cancelled anything: a dependency that changes while a request
   * is open starts a second one, and whichever *lands* last wins. So a slow
   * answer to a filter the reader has already moved past overwrites the fast
   * answer to the one they are looking at — the rows and the controls end up
   * describing different queries, and the page looks settled while being
   * wrong. Every surface built on this composable had it; the catalogue fixed
   * the same defect once, locally, in its own fetch.
   *
   * A sequence number rather than an `AbortController` because the generated
   * client takes no signal. The request still completes; its result is
   * discarded, which is what matters to the reader.
   */
  let seq = 0;

  async function run() {
    const mine = ++seq;
    const current = () => mine === seq;

    loading.value = true;
    error.value = null;
    try {
      const result = await fn();
      if (!current()) return;
      if (result.error) {
        error.value = extractMessage(result.error);
        data.value = null;
      } else if (typeof result.data === "string") {
        /*
         * Every endpoint in this API answers with JSON, so a string here means
         * something else replied: a reverse proxy's HTML error page, or the
         * SPA's own index.html when `/api` is not routed to the backend. The
         * generated client hands that text back as `data`, and a component then
         * calls `.filter()` on a string and throws during render — which kills
         * the page rather than showing anything.
         *
         * That is not hypothetical: it is what a production build served
         * without an API does, and it is why the CI rendered gate was scanning
         * an empty shell while reporting the pages clean.
         */
        error.value = t("common.apiNotJson");
        data.value = null;
      } else {
        data.value = result.data as T;
      }
    } catch (e) {
      if (!current()) return;
      error.value = extractMessage(e);
      data.value = null;
    } finally {
      // `loading` is the superseded call's too: clearing it from a stale
      // `finally` is what makes a spinner stop while the answer on screen is
      // still the wrong one.
      if (current()) loading.value = false;
    }
  }

  const watched = [...deps, tick];

  watchEffect(() => {
    watched.forEach((d) => d.value);
    void run();
  });

  function reload() {
    tick.value++;
  }

  return { data, error, loading, reload };
}
