import { describe, it, expect } from "vitest";
import { mount } from "@vue/test-utils";
import Label from "./Label.vue";

describe("Label", () => {
  it("renders slot content as a label element with base classes", () => {
    const wrapper = mount(Label, { slots: { default: "Username" } });
    expect(wrapper.text()).toBe("Username");
    expect(wrapper.element.tagName).toBe("LABEL");
    expect(wrapper.classes()).toContain("font-sans");
    expect(wrapper.classes()).not.toContain("uppercase");
  });

  it("passes through the for attribute", () => {
    const wrapper = mount(Label, {
      props: { for: "username" },
      slots: { default: "Username" },
    });
    expect(wrapper.attributes("for")).toBe("username");
  });

  it("merges a custom class with the base classes", () => {
    const wrapper = mount(Label, {
      props: { class: "my-label" },
      slots: { default: "x" },
    });
    expect(wrapper.classes()).toContain("my-label");
    expect(wrapper.classes()).toContain("font-sans");
  });
});
