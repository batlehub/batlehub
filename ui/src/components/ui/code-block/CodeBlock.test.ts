import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount } from "@vue/test-utils";
import { ref } from "vue";

const highlight = vi.fn();
const ready = ref(false);

vi.mock("@/composables/useShiki", () => ({
  useShiki: () => ({ highlight, ready }),
}));

import CodeBlock from "./CodeBlock.vue";

describe("CodeBlock", () => {
  beforeEach(() => {
    highlight.mockReset();
    ready.value = false;
  });

  it("renders the raw code in a <pre> fallback when highlight returns empty", () => {
    highlight.mockReturnValue("");
    const wrapper = mount(CodeBlock, { props: { code: "foo = 1", lang: "toml" } });

    expect(wrapper.find("pre").exists()).toBe(true);
    expect(wrapper.find("pre").text()).toBe("foo = 1");
    expect(wrapper.find(".shiki-wrapper").exists()).toBe(false);
  });

  it("renders highlighted html when highlight returns a non-empty string", () => {
    highlight.mockReturnValue('<span class="hl">foo = 1</span>');
    const wrapper = mount(CodeBlock, { props: { code: "foo = 1", lang: "toml" } });

    expect(wrapper.find(".shiki-wrapper").exists()).toBe(true);
    expect(wrapper.find("pre").exists()).toBe(false);
    expect(wrapper.html()).toContain("foo = 1");
  });

  it("calls highlight with the code and lang props on mount", () => {
    highlight.mockReturnValue("");
    mount(CodeBlock, { props: { code: "a = 1", lang: "toml" } });

    expect(highlight).toHaveBeenCalledWith("a = 1", "toml");
  });

  it("re-highlights when the code prop changes", async () => {
    highlight.mockReturnValue("");
    const wrapper = mount(CodeBlock, { props: { code: "a", lang: "toml" } });

    highlight.mockReturnValue("<span>b</span>");
    await wrapper.setProps({ code: "b" });

    expect(highlight).toHaveBeenCalledWith("b", "toml");
    expect(wrapper.find(".shiki-wrapper").exists()).toBe(true);
  });

  it("renders default slot content", () => {
    highlight.mockReturnValue("");
    const wrapper = mount(CodeBlock, {
      props: { code: "x", lang: "bash" },
      slots: { default: "<button>Copy</button>" },
    });

    expect(wrapper.find("button").exists()).toBe(true);
    expect(wrapper.text()).toContain("Copy");
  });

  it("renders a copy button only when given the text to copy", () => {
    highlight.mockReturnValue("");
    const plain = mount(CodeBlock, { props: { code: "a", lang: "bash" } });
    expect(plain.find("button").exists()).toBe(false);
    const copyable = mount(CodeBlock, { props: { code: "a ***", lang: "bash", copy: "a secret" } });
    expect(copyable.find("button").exists()).toBe(true);
  });
});
