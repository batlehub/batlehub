import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";

import Skeleton from "./Skeleton.vue";

describe("Skeleton", () => {
  it("renders the requested number of lines", () => {
    expect(
      mount(Skeleton, { props: { lines: 5 } }).findAll('[data-testid="skeleton-line"]'),
    ).toHaveLength(5);
  });

  /** A skeleton has nothing to say; the surface announces the real result. */
  it("is hidden from assistive tech", () => {
    expect(mount(Skeleton).attributes("aria-hidden")).toBe("true");
  });

  /** One Motion Rule: only an artifact resolving moves; a loading bar does not. */
  it("does not animate", () => {
    const bar = mount(Skeleton).find('[data-testid="skeleton-line"]');
    expect(bar.classes().some((c) => c.includes("animate"))).toBe(false);
  });

  /** Ragged widths read as text; identical bars read as a loading graphic. */
  it("varies line widths and shortens the last one", () => {
    const bars = mount(Skeleton, { props: { lines: 4 } }).findAll('[data-testid="skeleton-line"]');
    const widths = bars.map((b) => b.attributes("style"));
    expect(new Set(widths).size).toBeGreaterThan(1);
    expect(widths.at(-1)).toContain("62%");
  });
});
