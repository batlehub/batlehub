import { describe, expect, it } from "vitest";
import { nextTick } from "vue";
import { useDensity } from "./useDensity";

describe("useDensity", () => {
  it("writes data-density only for compact, and stores the choice", async () => {
    const { density } = useDensity();
    density.value = "compact";
    await nextTick();
    expect(document.documentElement.dataset.density).toBe("compact");
    expect(localStorage.getItem("batlehub.density")).toBe("compact");

    density.value = "comfortable";
    await nextTick();
    expect(document.documentElement.hasAttribute("data-density")).toBe(false);
  });
});
