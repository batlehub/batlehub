import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";

import EmptyState from "./EmptyState.vue";

/**
 * The distinction this component exists to keep: "nothing has ever been here"
 * and "your filter matched nothing" are different states. A user told "no
 * packages" while a filter is silently applied concludes the registry is broken.
 */
describe("EmptyState", () => {
  it("renders its title and description", () => {
    const w = mount(EmptyState, {
      props: { title: "No packages yet", description: "Publish one to see it here." },
    });
    expect(w.text()).toContain("No packages yet");
    expect(w.text()).toContain("Publish one to see it here.");
  });

  it("marks whether it is empty-by-filter, so the two states stay distinct", () => {
    const bare = mount(EmptyState, { props: { title: "No packages yet" } });
    const filtered = mount(EmptyState, { props: { title: "Nothing matches", filtered: true } });
    expect(bare.attributes("data-filtered")).toBe("false");
    expect(filtered.attributes("data-filtered")).toBe("true");
  });

  it("offers the recovery action its caller supplies", () => {
    const w = mount(EmptyState, {
      props: { title: "Nothing matches", filtered: true },
      slots: { action: '<button type="button">Clear filter</button>' },
    });
    expect(w.find("button").text()).toBe("Clear filter");
  });

  /** No nested card: cards inside cards are the lazy container. */
  it("sets a sentence as copy, with no bitmap heading, when there is no title", () => {
    const w = mount(EmptyState, { props: { description: "No changes recorded yet." } });
    expect(w.find(".font-display").exists()).toBe(false);
    expect(w.text()).toBe("No changes recorded yet.");
  });

  it("is a single ruled panel, not a card", () => {
    const w = mount(EmptyState, { props: { title: "x" } });
    expect(w.classes()).toContain("border");
    expect(w.findAll(".border")).toHaveLength(1);
  });
});
