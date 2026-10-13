import { describe, it, expect, vi } from "vitest";
import { ref } from "vue";
import { withSetup } from "@/test/withSetup";
import { useApi, extractMessage } from "./useApi";

describe("extractMessage", () => {
  it("returns 'Unknown error' for null/undefined", () => {
    expect(extractMessage(null)).toBe("Unknown error");
    expect(extractMessage(undefined)).toBe("Unknown error");
  });

  it("returns string errors directly", () => {
    expect(extractMessage("boom")).toBe("boom");
  });

  it("returns Error.message", () => {
    expect(extractMessage(new Error("oops"))).toBe("oops");
  });

  it("extracts .message from a plain object", () => {
    expect(extractMessage({ message: "bad request" })).toBe("bad request");
  });

  it("extracts .error from a plain object", () => {
    expect(extractMessage({ error: "forbidden" })).toBe("forbidden");
  });

  it("returns 'Unknown error' for an object with no message", () => {
    expect(extractMessage({ status: 500 })).toBe("Unknown error");
  });

  it("falls back to String(err) for other types", () => {
    expect(extractMessage(42)).toBe("42");
  });
});

describe("useApi", () => {
  it("starts loading and then populates data on success", async () => {
    const fn = vi.fn().mockResolvedValue({ data: { id: 1 } });
    const [state] = withSetup(() => useApi<{ id: number }>(fn));

    expect(state.loading.value).toBe(true);

    await vi.waitFor(() => expect(state.loading.value).toBe(false));
    expect(state.data.value).toEqual({ id: 1 });
    expect(state.error.value).toBeNull();
  });

  it("captures an error returned by fn", async () => {
    const fn = vi.fn().mockResolvedValue({ error: { message: "nope" } });
    const [state] = withSetup(() => useApi(fn));

    await vi.waitFor(() => expect(state.loading.value).toBe(false));
    expect(state.error.value).toBe("nope");
    expect(state.data.value).toBeNull();
  });

  it("captures a thrown error", async () => {
    const fn = vi.fn().mockRejectedValue(new Error("network down"));
    const [state] = withSetup(() => useApi(fn));

    await vi.waitFor(() => expect(state.loading.value).toBe(false));
    expect(state.error.value).toBe("network down");
    expect(state.data.value).toBeNull();
  });

  /**
   * The failure this pins killed the page rather than showing anything: a
   * reverse proxy (or `serve -s` in CI) answers /api with the SPA's own
   * index.html, the generated client hands that text back as `data`, and the
   * first component to call `.filter()` on it throws during render. The CI
   * rendered gate was scanning an empty shell and reporting it clean.
   */
  it("treats a non-JSON payload as an error rather than as data", async () => {
    const fn = vi.fn().mockResolvedValue({ data: "<!doctype html><html>…" });
    const [state] = withSetup(() => useApi(fn));

    await vi.waitFor(() => expect(state.loading.value).toBe(false));
    expect(state.data.value).toBeNull();
    expect(state.error.value).toContain("non-JSON");
  });

  /**
   * Nothing here cancelled anything, so whichever request *landed* last won —
   * and a slow answer to a filter the reader has already moved past would
   * overwrite the fast answer to the one on screen. The rows and the controls
   * then describe different queries, and the page looks settled while being
   * wrong. Every surface built on this composable had it.
   */
  describe("a superseded request", () => {
    it("does not overwrite the answer that superseded it", async () => {
      const dep = ref(0);
      let releaseSlow!: (v: unknown) => void;
      const fn = vi
        .fn()
        .mockReturnValueOnce(
          new Promise((resolve) => {
            releaseSlow = resolve;
          }),
        )
        .mockResolvedValue({ data: { v: "fast" } });
      const [state] = withSetup(() => useApi<{ v: string }>(fn, [dep]));

      dep.value++; // supersedes the one still in flight
      await vi.waitFor(() => expect(state.data.value?.v).toBe("fast"));

      releaseSlow({ data: { v: "slow" } });
      await Promise.resolve();
      await Promise.resolve();
      expect(state.data.value?.v).toBe("fast");
    });

    it("does not clear `loading` for the call still in flight", async () => {
      // The half that is easy to miss, and it needs both calls open at once:
      // the superseded one lands *while* its replacement is still waiting, and
      // its `finally` stops the spinner. The page then reads as settled with
      // the wrong answer — or with none.
      const dep = ref(0);
      let releaseFirst!: (v: unknown) => void;
      const fn = vi
        .fn()
        .mockReturnValueOnce(
          new Promise((resolve) => {
            releaseFirst = resolve;
          }),
        )
        .mockReturnValueOnce(new Promise(() => {}));
      const [state] = withSetup(() => useApi<{ v: string }>(fn, [dep]));

      dep.value++;
      await vi.waitFor(() => expect(fn).toHaveBeenCalledTimes(2));

      releaseFirst({ data: { v: "the abandoned one" } });
      await Promise.resolve();
      await Promise.resolve();
      expect(state.loading.value, "the second call is still open").toBe(true);
      expect(state.data.value).toBeNull();
    });

    it("does not surface an error from a call nobody is waiting for", async () => {
      const dep = ref(0);
      let rejectSlow!: (e: unknown) => void;
      const fn = vi
        .fn()
        .mockReturnValueOnce(
          new Promise((_, reject) => {
            rejectSlow = reject;
          }),
        )
        .mockResolvedValue({ data: { v: "fast" } });
      const [state] = withSetup(() => useApi<{ v: string }>(fn, [dep]));

      dep.value++;
      await vi.waitFor(() => expect(state.data.value?.v).toBe("fast"));

      rejectSlow(new Error("the abandoned one failed"));
      await Promise.resolve();
      await Promise.resolve();
      expect(state.error.value).toBeNull();
      expect(state.data.value?.v).toBe("fast");
    });
  });

  it("reload() re-invokes fn", async () => {
    const fn = vi.fn().mockResolvedValue({ data: { ok: true } });
    const [state] = withSetup(() => useApi(fn));

    await vi.waitFor(() => expect(fn).toHaveBeenCalledTimes(1));

    state.reload();
    await vi.waitFor(() => expect(fn).toHaveBeenCalledTimes(2));
  });
});
