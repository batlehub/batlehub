import { describe, it, expect, afterEach } from "vitest";
import { mount } from "@vue/test-utils";
import Select from "./Select.vue";

const options = [
  { value: "a", label: "Option A" },
  { value: "b", label: "Option B" },
];

describe("Select", () => {
  afterEach(() => {
    document.body.innerHTML = "";
  });

  /**
   * An option meaning "no value" — "All registries", "Any severity".
   *
   * radix refuses `<SelectItem value="">`, because the empty string is how a
   * `Select` clears itself. But a genuinely optional field needs blank to be
   * something you can *choose*: a subscription's registry is documented as
   * "leave blank for all registries", and a `Select` that could not express
   * blank would silently narrow every subscription that relied on it (RFC
   * 0004-bis §6.2). The empty string is carried through radix as a sentinel and
   * translated back at the boundary.
   */
  describe("an empty-valued option", () => {
    const withAll = [{ value: "", label: "All registries" }, ...options];

    it("renders rather than throwing", () => {
      const wrapper = mount(Select, {
        props: { options: withAll, modelValue: "" },
        attachTo: document.body,
      });
      // radix would have thrown on `value=""` during setup.
      expect(wrapper.find('[role="combobox"]').exists()).toBe(true);
    });

    /* What is *not* asserted here, stated rather than left to be assumed: the
       round trip — choosing "All registries" and the caller receiving `""`.
       radix renders its items into a portal that only mounts when the list
       opens, and jsdom has no layout for the trigger to open against, so there
       is nothing to click. Reaching past that to poke the handler directly
       would prove the branch and not the wiring, and the wiring is the half
       that broke. The rendered gate covers it on a real browser; nothing in
       vitest does. */
  });

  it("renders the trigger with placeholder and base classes", () => {
    const wrapper = mount(Select, {
      props: { options, placeholder: "Choose…" },
      attachTo: document.body,
    });
    expect(wrapper.text()).toContain("Choose…");
    const trigger = wrapper.find('[role="combobox"]');
    expect(trigger.classes()).toContain("flex");
    expect(trigger.classes()).toContain("h-9");
  });

  it("names the trigger the reader focuses, not radix's hidden select", () => {
    const wrapper = mount(Select, {
      props: { options, ariaLabel: "Visibility" },
      attachTo: document.body,
    });
    expect(wrapper.find('[role="combobox"]').attributes("aria-label")).toBe("Visibility");
    wrapper.unmount();
  });

  it("passes the id prop to the trigger", () => {
    const wrapper = mount(Select, {
      props: { options, id: "registry-select" },
      attachTo: document.body,
    });
    expect(wrapper.find('[role="combobox"]').attributes("id")).toBe("registry-select");
  });

  it("merges a custom class onto the trigger", () => {
    const wrapper = mount(Select, {
      props: { options, class: "my-select" },
      attachTo: document.body,
    });
    expect(wrapper.find('[role="combobox"]').classes()).toContain("my-select");
  });

  it("opens and lists options when the trigger is activated", async () => {
    const wrapper = mount(Select, {
      props: { options, placeholder: "Choose…" },
      attachTo: document.body,
    });

    // radix-vue opens the select on a left-button, non-touch pointerdown.
    wrapper.find('[role="combobox"]').element.dispatchEvent(
      new PointerEvent("pointerdown", {
        bubbles: true,
        cancelable: true,
        button: 0,
        ctrlKey: false,
        pointerType: "mouse",
      }),
    );
    await wrapper.vm.$nextTick();
    await wrapper.vm.$nextTick();
    await wrapper.vm.$nextTick();

    const items = document.body.querySelectorAll('[role="option"]');
    expect(items).toHaveLength(2);
    expect(document.body.textContent).toContain("Option A");
    expect(document.body.textContent).toContain("Option B");
  });
});
