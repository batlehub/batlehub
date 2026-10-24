import { describe, it, expect } from "vitest";
import { mount } from "@vue/test-utils";
import Card from "./Card.vue";
import CardHeader from "./CardHeader.vue";
import CardTitle from "./CardTitle.vue";
import CardDescription from "./CardDescription.vue";
import CardContent from "./CardContent.vue";
import CardFooter from "./CardFooter.vue";

describe("Card", () => {
  /** A region is a hairline above it — never a frame on a raised fill. */
  it("renders slot content under a top hairline, with no frame or fill", () => {
    const wrapper = mount(Card, { slots: { default: "Body" } });
    expect(wrapper.text()).toBe("Body");
    expect(wrapper.element.tagName).toBe("DIV");
    expect(wrapper.classes()).toContain("border-t");
    expect(wrapper.classes()).toContain("border-rule-soft");
    expect(wrapper.classes()).not.toContain("border");
    expect(wrapper.classes()).not.toContain("bg-card");
  });

  it("merges a custom class", () => {
    const wrapper = mount(Card, { props: { class: "my-card" }, slots: { default: "Body" } });
    expect(wrapper.classes()).toContain("my-card");
    expect(wrapper.classes()).toContain("border-t");
  });
});

describe("CardHeader", () => {
  it("renders slot content in a div with base classes", () => {
    const wrapper = mount(CardHeader, { slots: { default: "Header" } });
    expect(wrapper.text()).toBe("Header");
    expect(wrapper.element.tagName).toBe("DIV");
    expect(wrapper.classes()).toContain("flex");
    expect(wrapper.classes()).toContain("flex-col");
  });

  it("merges a custom class", () => {
    const wrapper = mount(CardHeader, { props: { class: "my-header" }, slots: { default: "x" } });
    expect(wrapper.classes()).toContain("my-header");
  });
});

describe("CardTitle", () => {
  /**
   * `h2` by default, because a card usually sits directly under the page `h1`.
   * It was a hardcoded `h3`, which skipped a level on every such page.
   */
  it("renders slot content in an h2 with base classes", () => {
    const wrapper = mount(CardTitle, { slots: { default: "Title" } });
    expect(wrapper.text()).toBe("Title");
    expect(wrapper.element.tagName).toBe("H2");
    expect(wrapper.classes()).toContain("font-mono");
    // The Head step, regular — not 24px bold competing with the page h1.
    expect(wrapper.classes()).toContain("text-lg");
    expect(wrapper.classes()).not.toContain("font-bold");
  });

  it("takes a deeper level when the card is nested under a section heading", () => {
    const wrapper = mount(CardTitle, { props: { as: "h3" }, slots: { default: "Title" } });
    expect(wrapper.element.tagName).toBe("H3");
  });

  it("merges a custom class", () => {
    const wrapper = mount(CardTitle, { props: { class: "my-title" }, slots: { default: "x" } });
    expect(wrapper.classes()).toContain("my-title");
  });
});

describe("CardDescription", () => {
  it("renders slot content in a p with base classes", () => {
    const wrapper = mount(CardDescription, { slots: { default: "Description" } });
    expect(wrapper.text()).toBe("Description");
    expect(wrapper.element.tagName).toBe("P");
    expect(wrapper.classes()).toContain("text-muted-foreground");
  });

  it("merges a custom class", () => {
    const wrapper = mount(CardDescription, {
      props: { class: "my-desc" },
      slots: { default: "x" },
    });
    expect(wrapper.classes()).toContain("my-desc");
  });
});

describe("CardContent", () => {
  it("renders slot content in a div with base classes", () => {
    const wrapper = mount(CardContent, { slots: { default: "Content" } });
    expect(wrapper.text()).toBe("Content");
    expect(wrapper.element.tagName).toBe("DIV");
    // Full measure: no side padding, only the gap before the next region.
    expect(wrapper.classes()).toContain("pb-6");
    expect(wrapper.classes()).not.toContain("p-6");
  });

  it("merges a custom class", () => {
    const wrapper = mount(CardContent, {
      props: { class: "my-content" },
      slots: { default: "x" },
    });
    expect(wrapper.classes()).toContain("my-content");
  });
});

describe("CardFooter", () => {
  it("renders slot content in a div with base classes", () => {
    const wrapper = mount(CardFooter, { slots: { default: "Footer" } });
    expect(wrapper.text()).toBe("Footer");
    expect(wrapper.element.tagName).toBe("DIV");
    expect(wrapper.classes()).toContain("flex");
    expect(wrapper.classes()).toContain("items-center");
  });

  it("merges a custom class", () => {
    const wrapper = mount(CardFooter, {
      props: { class: "my-footer" },
      slots: { default: "x" },
    });
    expect(wrapper.classes()).toContain("my-footer");
  });
});
