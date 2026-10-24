import { describe, it, expect } from "vitest";
import { mount } from "@vue/test-utils";
import Alert from "./Alert.vue";

describe("Alert", () => {
  it("renders slot content as a polite status by default", () => {
    const wrapper = mount(Alert, { slots: { default: "Heads up" } });
    expect(wrapper.text()).toBe("Heads up");
    expect(wrapper.attributes("role")).toBe("status");
  });

  it("interrupts only for a refusal", () => {
    const wrapper = mount(Alert, { props: { variant: "destructive" }, slots: { default: "No" } });
    expect(wrapper.attributes("role")).toBe("alert");
    const ok = mount(Alert, { props: { variant: "success" }, slots: { default: "Done" } });
    expect(ok.attributes("role")).toBe("status");
  });

  it("applies the default variant classes", () => {
    const wrapper = mount(Alert, { slots: { default: "Info" } });
    expect(wrapper.classes()).toContain("bg-background");
    expect(wrapper.classes()).toContain("text-foreground");
  });

  it("applies the destructive variant classes", () => {
    const wrapper = mount(Alert, {
      props: { variant: "destructive" },
      slots: { default: "Error" },
    });
    expect(wrapper.classes()).toContain("text-destructive");
  });

  it("applies the success variant classes", () => {
    const wrapper = mount(Alert, {
      props: { variant: "success" },
      slots: { default: "Done" },
    });
    expect(wrapper.classes()).toContain("text-foreground");
  });

  it("merges a custom class with the variant classes", () => {
    const wrapper = mount(Alert, {
      props: { class: "my-custom-class" },
      slots: { default: "Info" },
    });
    expect(wrapper.classes()).toContain("my-custom-class");
    expect(wrapper.classes()).toContain("bg-background");
  });
});
