import { onScopeDispose, ref, watch, type Ref } from "vue";

/** RFC 0042 §4.2's bounds. */
export const POLL_INTERVAL_MS = 5_000;
export const POLL_MAX_MS = 10 * 60_000;
export const POLL_MAX_BACKOFF_MS = 60_000;
export const POLL_MAX_FAILURES = 3;

/** Why a poll stopped for good, until `restart()`. */
export type PollStop = "timeout" | "failed";

/** A failed tick's `Retry-After`, in seconds, when the error carries a response. */
function retryAfterMs(e: unknown): number {
  const header = (e as { response?: Response })?.response?.headers?.get?.("Retry-After");
  const secs = header ? Number(header) : NaN;
  return Number.isFinite(secs) && secs > 0 ? secs * 1000 : 0;
}

/**
 * The bounded poll of RFC 0042 §4.2: `fn` every 5 s while `active` is true.
 *
 * - paused while the document is hidden, resumed when it is shown again;
 * - stopped after 10 minutes (`stopped = "timeout"`), so a stuck job cannot
 *   keep a tab polling forever;
 * - a failed tick (a throw) backs off ×2 up to 60 s, honouring `Retry-After`,
 *   and three in a row stop it (`stopped = "failed"`).
 *
 * `restart()` is the page's [Refresh]: it runs `fn` now and starts again.
 */
export function usePoll(fn: () => Promise<void>, active: Ref<boolean>) {
  const stopped = ref<PollStop | null>(null);
  let timer: ReturnType<typeof setTimeout> | null = null;
  let startedAt = 0;
  let failures = 0;
  let delay = POLL_INTERVAL_MS;

  const hidden = () => typeof document !== "undefined" && document.visibilityState === "hidden";

  function clear() {
    if (timer) clearTimeout(timer);
    timer = null;
  }

  function schedule() {
    clear();
    if (!active.value || stopped.value || hidden()) return;
    timer = setTimeout(tick, delay);
  }

  async function tick() {
    timer = null;
    if (!active.value || hidden()) return;
    if (Date.now() - startedAt >= POLL_MAX_MS) {
      stopped.value = "timeout";
      return;
    }
    try {
      await fn();
      failures = 0;
      delay = POLL_INTERVAL_MS;
    } catch (e) {
      failures += 1;
      if (failures >= POLL_MAX_FAILURES) {
        stopped.value = "failed";
        return;
      }
      delay = Math.max(Math.min(delay * 2, POLL_MAX_BACKOFF_MS), retryAfterMs(e));
    }
    schedule();
  }

  function start() {
    startedAt = Date.now();
    failures = 0;
    delay = POLL_INTERVAL_MS;
    stopped.value = null;
    schedule();
  }

  function restart() {
    start();
    void fn().catch(() => {});
  }

  watch(active, (on) => (on ? start() : clear()), { immediate: true });

  const onVisibility = () => (hidden() ? clear() : schedule());
  if (typeof document !== "undefined") document.addEventListener("visibilitychange", onVisibility);
  onScopeDispose(() => {
    clear();
    if (typeof document !== "undefined")
      document.removeEventListener("visibilitychange", onVisibility);
  });

  return { stopped, restart };
}
