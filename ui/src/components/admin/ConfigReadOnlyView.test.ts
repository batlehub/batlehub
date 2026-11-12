import { describe, it, expect, vi } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";

import ConfigReadOnlyView from "./ConfigReadOnlyView.vue";

const TOML = '[server]\nport = 8080\n\n[[registries]]\nname = "npm"\n';

const mountView = (props: { content: string; error?: string | null; jumpTo?: string }) =>
  mount(ConfigReadOnlyView, { props });

/**
 * RFC 0003 §6.4: when the config is mounted read-only this is a *different*
 * screen, not a disabled editor. So the assertions are about what it is not —
 * no textarea, no Apply — as much as about what it shows.
 */
describe("ConfigReadOnlyView", () => {
  it("shows the running config without offering to edit it", () => {
    const wrapper = mountView({ content: TOML });
    expect(wrapper.text()).toContain("port = 8080");
    expect(wrapper.find("textarea").exists()).toBe(false);
    expect(wrapper.findAll("button").some((b) => /apply|validate/i.test(b.text()))).toBe(false);
  });

  it("says where the config actually comes from", () => {
    const text = mountView({ content: TOML }).text();
    expect(text).toContain("read-only");
    expect(text).toContain("ConfigMap");
  });

  it("offers the config for copying, since it cannot be edited here", () => {
    const wrapper = mountView({ content: TOML });
    const copy = wrapper.findAll("button").find((b) => /copy/i.test(b.text() + b.html()));
    expect(copy).toBeDefined();
  });

  /** RFC 0046 M5, read-only mount: no caret, so the line is shown instead. */
  it("scrolls to and highlights the registry's line, then lets it go", async () => {
    vi.useFakeTimers();
    try {
      const wrapper = mount(ConfigReadOnlyView, {
        props: { content: TOML, jumpTo: "npm" },
        attachTo: document.body,
        global: {
          // The highlighted code renders one `.line` per source line.
          stubs: {
            CodeBlock: {
              props: ["code"],
              template: `<pre><span v-for="(l, i) in code.split('\\n')" :key="i" class="line">{{ l }}</span></pre>`,
            },
          },
        },
      });
      await flushPromises();
      // One poll: the code renders after the view does, as it does in a browser.
      vi.advanceTimersByTime(100);
      const line = wrapper.findAll(".line")[4]!;
      expect(line.text()).toBe('name = "npm"');
      expect(line.classes()).toContain("outline");
      expect(wrapper.get('[data-testid="jump-notice"]').text()).toBe("Jumped to registry “npm”.");
      vi.advanceTimersByTime(2000);
      expect(line.classes()).not.toContain("outline");
      wrapper.unmount();
    } finally {
      vi.useRealTimers();
    }
  });

  it("says the registry lives in another layer when this file has no block for it", () => {
    const text = mountView({ content: TOML, jumpTo: "pypi" }).text();
    expect(text).toContain("defined in another config layer");
  });

  it("shows the load error instead of an empty config block", () => {
    const wrapper = mountView({ content: "", error: "permission denied reading config.toml" });
    expect(wrapper.text()).toContain("permission denied reading config.toml");
    expect(wrapper.find("pre").exists()).toBe(false);
  });
});
