import { describe, expect, it } from "vitest";
import { mount } from "@vue/test-utils";
import Listbox from "./Listbox.vue";

describe("Listbox", () => {
  /**
   * RFC 0046 §7.1 T4. `vue/no-v-html` is off for `components/ui/**`, where this
   * file lives, so lint does not guard it: this test does. A registry name is
   * data an operator typed, and it must render as text.
   */
  it("renders a markup-shaped label as text (T4)", () => {
    const evil = '<img src=x onerror="window.__pwned=1">';
    const w = mount(Listbox, {
      props: { id: "lb", items: [{ key: "a", label: evil, hint: evil }], activeIndex: -1 },
    });
    expect(w.find("img").exists()).toBe(false);
    expect(w.text()).toContain(evil);
  });

  it("draws an option with an href as a link, and groups consecutive items", () => {
    const w = mount(Listbox, {
      props: {
        id: "lb",
        activeIndex: 1,
        items: [
          { key: "a", label: "A", href: "/a", group: "G" },
          { key: "b", label: "B", href: "/b", group: "G" },
          { key: "c", label: "C" },
        ],
      },
    });
    const options = w.findAll('[role="option"]');
    expect(options.map((o) => o.element.tagName)).toEqual(["A", "A", "DIV"]);
    expect(options[0]?.attributes("href")).toBe("/a");
    expect(options[1]?.attributes("aria-selected")).toBe("true");
    expect(options.map((o) => o.attributes("id"))).toEqual(["lb-opt-0", "lb-opt-1", "lb-opt-2"]);
    expect(w.findAll('[role="group"]')).toHaveLength(1);
  });
});
