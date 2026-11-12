import { watch } from "vue";
import { useRoute } from "vue-router";

/**
 * `?registry=` on a registry-scoped page (RFC 0046 §4.2, §6.3), held until the
 * page's own list first settles, then applied once if the list names it.
 *
 * Read on mount against a list still loading, the name would be dropped — the
 * slower the API, the likelier, which is exactly during an incident. A name the
 * settled list lacks is dropped silently (the page's normal "choose a registry"
 * state); a list that fails never settles, so the value is kept and the page
 * shows its own error. The name is only ever compared with values the page
 * fetched itself, never interpolated into a request (§7.1 T5).
 */
export function usePreselectRegistry(
  names: () => readonly string[],
  settled: () => boolean,
  apply: (name: string) => void,
) {
  // `?.`: a page mounted without a router (a unit test) simply has no query.
  const raw = useRoute()?.query.registry;
  const wanted = Array.isArray(raw) ? raw[0] : raw;
  if (typeof wanted !== "string" || wanted === "") return;
  let done = false;
  watch(
    settled,
    (isSettled) => {
      if (!isSettled || done) return;
      done = true;
      if (names().includes(wanted)) apply(wanted);
    },
    { immediate: true },
  );
}
