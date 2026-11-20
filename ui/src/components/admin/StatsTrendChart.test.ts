import { describe, expect, it } from "vitest";
import { mount } from "@vue/test-utils";
import StatsTrendChart from "./StatsTrendChart.vue";

/**
 * RFC 0050 §11: the criteria the SVG-free chart must meet, failing any one of
 * which is the trigger to move to a charting library (§12). Criterion 2, the
 * table, is the Trends page's and tested there; criterion 4 (3:1 contrast) is
 * the rendered gate's — the marks are `foreground`, never a tint.
 */
describe("StatsTrendChart (RFC 0050 §11)", () => {
  it("1 · states min, max and last value in its text alternative", () => {
    const w = mount(StatsTrendChart, { props: { label: "Held", values: [3, null, 9, 5] } });
    const alt = w.get('[role="img"]').attributes("aria-label") ?? "";
    expect(alt).toContain("min 3");
    expect(alt).toContain("max 9");
    expect(alt).toContain("last 5");
  });

  it("3 · tells two series apart by a marker, not by colour, with a text legend", () => {
    const w = mount(StatsTrendChart, {
      props: {
        label: "Scans",
        series: [
          { label: "done", values: [2, 4] },
          { label: "failed", values: [1, 1] },
        ],
      },
    });
    const bars = w.findAll('[data-testid="bar"]');
    expect(bars).toHaveLength(4);
    expect(bars[0]!.classes()).toContain("bg-foreground");
    expect(bars[1]!.classes()).toContain("border");
    expect(bars[1]!.classes()).not.toContain("bg-foreground");
    expect(w.text()).toContain("done");
    expect(w.text()).toContain("failed");
  });

  it("4 · draws marks in full ink, not a tint", () => {
    const w = mount(StatsTrendChart, { props: { label: "x", values: [1] } });
    expect(
      w
        .get('[data-testid="bar"]')
        .classes()
        .some((c) => c.includes("/")),
    ).toBe(false);
  });

  it("5 · has no transition to animate", () => {
    const w = mount(StatsTrendChart, { props: { label: "x", values: [1, 2] } });
    expect(w.html()).not.toMatch(/transition|animate/);
  });

  it("6 · draws a day without data as a gap and announces it, never as zero", () => {
    const w = mount(StatsTrendChart, { props: { label: "Held", values: [null, 4, null] } });
    expect(w.findAll('[data-testid="gap"]')).toHaveLength(2);
    expect(w.findAll('[data-testid="bar"]')).toHaveLength(1);
    const alt = w.get('[role="img"]').attributes("aria-label") ?? "";
    expect(alt).toContain("2 days without data");
    expect(alt).not.toContain("min 0");
  });

  it("says 'no data' when a series has none at all", () => {
    const w = mount(StatsTrendChart, { props: { label: "Held", values: [null, null] } });
    expect(w.get('[role="img"]').attributes("aria-label")).toBe("Held: no data");
  });

  it("counts the gaps of an hourly series in hours, not days", () => {
    const w = mount(StatsTrendChart, {
      props: { label: "Hit rate", values: [1, null, null], unit: "hour" },
    });
    const alt = w.get('[role="img"]').attributes("aria-label") ?? "";
    expect(alt).toContain("2 hours without data");
    expect(alt).not.toContain("day");
  });
});
