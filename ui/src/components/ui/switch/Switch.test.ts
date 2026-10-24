import { describe, it, expect } from "vitest";
import { mount } from "@vue/test-utils";
import Switch from "./Switch.vue";

describe("Switch", () => {
  it("renders a button with role=switch", () => {
    const wrapper = mount(Switch);
    expect(wrapper.element.tagName).toBe("BUTTON");
    expect(wrapper.attributes("role")).toBe("switch");
  });

  it("reflects modelValue via aria-checked and background class", () => {
    const off = mount(Switch, { props: { modelValue: false } });
    expect(off.attributes("aria-checked")).toBe("false");
    expect(off.classes()).toContain("bg-transparent");

    const on = mount(Switch, { props: { modelValue: true } });
    expect(on.attributes("aria-checked")).toBe("true");
    // On is ink, not crimson: crimson has four jobs and "enabled" is not one.
    expect(on.classes()).toContain("bg-foreground");
    expect(on.classes()).not.toContain("bg-primary");
  });

  it("emits update:modelValue with the toggled value when clicked", async () => {
    const wrapper = mount(Switch, { props: { modelValue: false } });
    await wrapper.trigger("click");
    expect(wrapper.emitted("update:modelValue")?.[0]).toEqual([true]);
  });

  it("does not emit when disabled", async () => {
    const wrapper = mount(Switch, { props: { modelValue: false, disabled: true } });
    expect(wrapper.attributes("disabled")).toBeDefined();
    await wrapper.trigger("click");
    expect(wrapper.emitted("update:modelValue")).toBeUndefined();
  });
});
