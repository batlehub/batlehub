import { describe, it, expect } from "vitest";
import { mount } from "@vue/test-utils";
import { PageHeader } from ".";

describe("PageHeader", () => {
  it("renders the title and description", () => {
    const wrapper = mount(PageHeader, { props: { title: "Health", description: "Live status" } });
    expect(wrapper.find("h1").text()).toBe("Health");
    expect(wrapper.text()).toContain("Live status");
  });

  it("renders an h2 inside a hub, whose layout already owns the h1", () => {
    const wrapper = mount(PageHeader, { props: { title: "Tokens", as: "h2" } });
    expect(wrapper.find("h1").exists()).toBe(false);
    expect(wrapper.find("h2").text()).toBe("Tokens");
  });

  it("uses the plain style by default", () => {
    const wrapper = mount(PageHeader, { props: { title: "Health" } });
    expect(wrapper.find("h1").classes()).toContain("text-2xl");
    expect(wrapper.find("h1").classes()).not.toContain("font-display");
  });

  // The display variant is the only step above the data ramp, so it is the one
  // place the bitmap face is opted into.
  it("applies the display variant", () => {
    const wrapper = mount(PageHeader, { props: { title: "Health", variant: "display" } });
    expect(wrapper.find("h1").classes()).toContain("font-display");
  });

  // The Flat-At-Rest Rule: no title in this system carries a glow. Pinned so a
  // reinstated text-shadow fails here rather than in a rendered-page scan.
  it("never glows in either variant", () => {
    for (const variant of ["default", "display"] as const) {
      const wrapper = mount(PageHeader, { props: { title: "Health", variant } });
      expect(wrapper.find("h1").classes()).not.toContain("cyber-text-glow");
    }
  });

  it("renders the actions slot", () => {
    const wrapper = mount(PageHeader, {
      props: { title: "Health" },
      slots: { actions: "<button>Refresh</button>" },
    });
    expect(wrapper.text()).toContain("Refresh");
  });

  it("renders no button when the actions slot is absent", () => {
    const wrapper = mount(PageHeader, { props: { title: "Health" } });
    expect(wrapper.find("button").exists()).toBe(false);
  });

  it("renders the title slot instead of the title prop when provided", () => {
    const wrapper = mount(PageHeader, {
      slots: { title: '<svg class="icon" /> Custom Title' },
    });
    expect(wrapper.find("h1").text()).toContain("Custom Title");
    expect(wrapper.find("h1 svg.icon").exists()).toBe(true);
  });

  it("follows the title with its count, and shows none for zero", () => {
    expect(
      mount(PageHeader, { props: { title: "Audit", count: 42 } })
        .find("h1")
        .text(),
    ).toBe("Audit(42)");
    expect(
      mount(PageHeader, { props: { title: "Audit", count: 0 } })
        .find("h1")
        .text(),
    ).toBe("Audit");
  });
});
