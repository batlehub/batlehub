import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { effectScope, nextTick, ref } from "vue";

import { usePoll, POLL_INTERVAL_MS, POLL_MAX_MS } from "./usePoll";

function setVisibility(state: "visible" | "hidden") {
  Object.defineProperty(document, "visibilityState", { configurable: true, get: () => state });
  document.dispatchEvent(new Event("visibilitychange"));
}

function poll(fn: () => Promise<void>, on = true) {
  const scope = effectScope();
  const active = ref(on);
  const api = scope.run(() => usePoll(fn, active))!;
  return { ...api, active, scope };
}

describe("usePoll (RFC 0044 §4.2)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    setVisibility("visible");
  });
  afterEach(() => {
    vi.useRealTimers();
    setVisibility("visible");
  });

  it("ticks every 5 s while active, and not at all while inactive", async () => {
    const fn = vi.fn().mockResolvedValue(undefined);
    const p = poll(fn, false);
    await vi.advanceTimersByTimeAsync(POLL_INTERVAL_MS * 3);
    expect(fn).not.toHaveBeenCalled();
    p.active.value = true;
    await nextTick();
    await vi.advanceTimersByTimeAsync(POLL_INTERVAL_MS * 2);
    expect(fn).toHaveBeenCalledTimes(2);
    p.active.value = false;
    await nextTick();
    await vi.advanceTimersByTimeAsync(POLL_INTERVAL_MS * 3);
    expect(fn).toHaveBeenCalledTimes(2);
    p.scope.stop();
  });

  it("pauses while hidden and resumes when shown", async () => {
    const fn = vi.fn().mockResolvedValue(undefined);
    const p = poll(fn);
    setVisibility("hidden");
    await vi.advanceTimersByTimeAsync(POLL_INTERVAL_MS * 4);
    expect(fn).not.toHaveBeenCalled();
    setVisibility("visible");
    await vi.advanceTimersByTimeAsync(POLL_INTERVAL_MS);
    expect(fn).toHaveBeenCalledTimes(1);
    p.scope.stop();
  });

  it("stops after 10 minutes, and restart() runs at once and starts again", async () => {
    const fn = vi.fn().mockResolvedValue(undefined);
    const p = poll(fn);
    await vi.advanceTimersByTimeAsync(POLL_MAX_MS + POLL_INTERVAL_MS);
    expect(p.stopped.value).toBe("timeout");
    const calls = fn.mock.calls.length;
    await vi.advanceTimersByTimeAsync(POLL_INTERVAL_MS * 5);
    expect(fn).toHaveBeenCalledTimes(calls);
    p.restart();
    expect(p.stopped.value).toBeNull();
    expect(fn).toHaveBeenCalledTimes(calls + 1);
    p.scope.stop();
  });

  it("backs off ×2 on failure, honours Retry-After, and stops after 3 failures", async () => {
    const retry = Object.assign(new Error("503"), {
      response: new Response(null, { status: 503, headers: { "Retry-After": "30" } }),
    });
    const fn = vi.fn().mockRejectedValueOnce(new Error("500")).mockRejectedValueOnce(retry);
    const p = poll(fn);
    await vi.advanceTimersByTimeAsync(POLL_INTERVAL_MS); // fail 1 → next in 10 s
    expect(fn).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(POLL_INTERVAL_MS * 2 - 1);
    expect(fn).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1); // fail 2 → max(20 s, 30 s)
    expect(fn).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(29_999);
    expect(fn).toHaveBeenCalledTimes(2);
    fn.mockRejectedValue(new Error("500"));
    await vi.advanceTimersByTimeAsync(1); // fail 3 → stop
    expect(fn).toHaveBeenCalledTimes(3);
    expect(p.stopped.value).toBe("failed");
    await vi.advanceTimersByTimeAsync(120_000);
    expect(fn).toHaveBeenCalledTimes(3);
    p.scope.stop();
  });
});
