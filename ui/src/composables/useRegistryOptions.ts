import { computed } from "vue";

import { listRegistries } from "@/client/sdk.gen";
import type { RegistryInfo } from "@/client/types.gen";
import { useApi } from "./useApi";
import { useAuth } from "./useAuth";

/**
 * The registries this viewer can see, as `Select` options.
 *
 * RFC 0004-bis §2.8: the same concept was a closed list on one page and a
 * free-text box on another. Four fields asked an operator to type a registry
 * name — and the placeholder in each guessed a *different* convention
 * (`e.g. crates-io`, `github`, `npm`, `e.g. my-cargo`), which is what a field
 * looks like when nobody could check their answer. A registry list is a handful
 * of entries and is fetched on nearly every page already.
 *
 * `listRegistries` rather than `registryHealth`: this is role-filtered and
 * reachable by an anonymous visitor, so the public `/tools/access-check` and
 * the admin pages can share one source instead of two that must agree.
 */
export function useRegistryOptions() {
  const { token } = useAuth();

  const { data, loading, error, reload } = useApi<RegistryInfo[]>(
    () => listRegistries() as Promise<{ data?: unknown; error?: unknown }>,
    [token],
  );

  const registries = computed(() => data.value ?? []);

  const options = computed(() => registries.value.map((r) => ({ value: r.name, label: r.name })));

  /** The registry's declared type, for a field whose shape depends on it. */
  const typeOf = (name: string): string | undefined =>
    registries.value.find((r) => r.name === name)?.type;

  // `reload`: the palette refetches when opened after a failed load (RFC 0046 §4.4).
  return { registries, options, typeOf, loading, error, reload };
}
