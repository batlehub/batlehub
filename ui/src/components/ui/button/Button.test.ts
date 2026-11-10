import { describe, it, expect } from "vitest";
import { mount } from "@vue/test-utils";
import { Button } from ".";

describe("Button", () => {
  it("renders slot content", () => {
    const wrapper = mount(Button, { slots: { default: "Click me" } });
    expect(wrapper.text()).toBe("Click me");
    expect(wrapper.element.tagName).toBe("BUTTON");
  });

  it("applies the default variant and size classes", () => {
    const wrapper = mount(Button, { slots: { default: "Go" } });
    expect(wrapper.classes()).toContain("bg-primary");
    expect(wrapper.classes()).toContain("h-9");
  });

  it("applies variant and size props", () => {
    const wrapper = mount(Button, {
      props: { variant: "destructive", size: "sm" },
      slots: { default: "Delete" },
    });
    expect(wrapper.classes()).toContain("bg-destructive");
    expect(wrapper.classes()).toContain("h-8");
  });

  it("disables the button when the disabled prop is set", () => {
    const wrapper = mount(Button, { props: { disabled: true }, slots: { default: "Go" } });
    expect(wrapper.attributes("disabled")).toBeDefined();
  });

  it("forwards extra attributes such as click handlers", async () => {
    let clicked = 0;
    const wrapper = mount(Button, {
      attrs: { onClick: () => clicked++ },
      slots: { default: "Go" },
    });
    await wrapper.trigger("click");
    expect(clicked).toBe(1);
  });

  it("merges a custom class with the variant classes", () => {
    const wrapper = mount(Button, {
      props: { class: "my-custom-class" },
      slots: { default: "Go" },
    });
    expect(wrapper.classes()).toContain("my-custom-class");
    expect(wrapper.classes()).toContain("bg-primary");
  });

  const classesOf = (variant: "default" | "destructive" | "outline" | "ghost") =>
    mount(Button, { props: { variant }, slots: { default: "x" } }).classes();

  /**
   * DESIGN.md, Buttons: "disabled drops the fill entirely and becomes a dim
   * outlined control (crimson never appears in a disabled state)", and §Colour
   * repeats it: "don't let it appear in a disabled control".
   *
   * `disabled:opacity-50` used to sit on the shared base, which dimmed every
   * variant uniformly — so the two filled ones kept their crimson underneath
   * at 2.07:1 and the rule was broken by the one class that looked like it was
   * being careful.
   */
  describe("disabled", () => {
    it("drops the fill on the filled variants instead of dimming it", () => {
      for (const variant of ["default", "destructive"] as const) {
        const classes = classesOf(variant);
        expect(classes, variant).toContain("disabled:bg-transparent");
        expect(
          classes.filter((c) => c.startsWith("disabled:opacity-")),
          variant,
        ).toEqual([]);
      }
    });

    it("dims the unfilled ones, which is what the rule gives them", () => {
      for (const variant of ["outline", "ghost"] as const) {
        expect(classesOf(variant), variant).toContain("disabled:opacity-50");
      }
    });
  });

  /**
   * DESIGN.md's "Secondary — the control" (`.ctl`), which
   * `ui/design-proof/index.html:186-191` implements:
   *
   *     .ctl       { background: transparent; border: 1px solid var(--rule-strong);
   *                  color: var(--ink-dim) }
   *     .ctl:hover { color: var(--ink); border-color: var(--ink-dim) }
   *
   * The variant was `border-primary/40 text-primary` — crimson at 40% — which
   * is not the specified control and measured 1.70:1 in dark, 2.14:1 in light,
   * for the boundary of an interactive element.
   */
  describe("the outline variant is the specified control", () => {
    it("rests on --rule-strong and --ink-dim, at full strength", () => {
      const classes = classesOf("outline");
      expect(classes).toContain("border-border");
      expect(classes).toContain("text-muted-foreground");
      expect(classes.filter((c) => /^border-\w+\//.test(c))).toEqual([]);
    });

    it("carries no crimson", () => {
      // The One Synthetic Rule's discipline: exactly one filled crimson action
      // per view, and this is not it.
      expect(classesOf("outline").filter((c) => /(primary|destructive)/.test(c))).toEqual([]);
    });

    it("lifts both the ink and the border on hover", () => {
      // Two channels, and it needs both: `hover:bg-accent` resolved to
      // `--ground-raised`, which `tokens.css` annotates as "1.06:1 —
      // confirmation, not elevation". The fill was imperceptible, so the
      // border alpha was doing all the work.
      const classes = classesOf("outline");
      expect(classes).toContain("hover:text-foreground");
      expect(classes).toContain("hover:border-muted-foreground");
      expect(classes).not.toContain("hover:bg-accent");
    });

    it("turns copper when pressed", () => {
      const classes = classesOf("outline");
      expect(classes).toContain("aria-pressed:text-foreground");
      expect(classes).toContain("aria-pressed:border-copper");
    });
  });
});
