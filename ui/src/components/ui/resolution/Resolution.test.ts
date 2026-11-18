import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";

import Resolution from "./Resolution.vue";
import type { ResolutionState } from "./index";

/**
 * The component is a transcription of DESIGN.md's six-state table, so the tests
 * assert against that table rather than against the implementation — a test that
 * re-derives the counts from `STATES` would pass no matter what the table said.
 *
 * The numbers below are copied from DESIGN.md's "Resolution as State" section
 * and from `ui/design-proof/index.html`'s `STATES` object, which agree.
 */
const cells = (wrapper: ReturnType<typeof mount>) =>
  wrapper.find('[data-testid="resolution-matrix"]').findAll("i");

const lit = (wrapper: ReturnType<typeof mount>) =>
  cells(wrapper).filter((c) => c.classes().includes("opacity-100")).length;

const mountState = (state: ResolutionState) =>
  mount(Resolution, { props: { state, label: state } });

describe("Resolution", () => {
  it.each([
    ["cached", 9, 9],
    ["stale", 9, 8],
    ["held", 4, 3],
    ["pending", 4, 2],
    ["yanked", 4, 1],
    ["blocked", 4, 4],
  ] as const)("renders %s as %i cells with %i lit", (state, total, on) => {
    const w = mountState(state);
    expect(cells(w)).toHaveLength(total);
    expect(lit(w)).toBe(on);
  });

  /**
   * "Held and verified renders at full resolution; what it does not renders
   * coarse." Only `cached` and `stale` are held, so only they are fine — this is
   * the distinction the whole device exists to make, and a nine-cell `pending`
   * would quietly claim BatleHub has the artifact.
   */
  it("uses the fine matrix only for the two held states", () => {
    for (const s of ["cached", "stale"] as const) {
      expect(cells(mountState(s))).toHaveLength(9);
    }
    for (const s of ["held", "pending", "yanked", "blocked"] as const) {
      expect(cells(mountState(s))).toHaveLength(4);
    }
  });

  /**
   * DESIGN.md says stale is "8 of 9, centre out" — *which* cell is dark is the
   * specification, not merely how many. A stale matrix with a corner missing
   * would satisfy a count-only test and read as a different mark.
   */
  it("darkens the centre cell for stale, not an edge", () => {
    const c = cells(mountState("stale"));
    expect(c[4].classes()).toContain("opacity-[.18]");
    expect(c.filter((_, i) => i !== 4).every((x) => x.classes().includes("opacity-100"))).toBe(
      true,
    );
  });

  it.each([
    ["cached", "text-ok"],
    ["stale", "text-copper"],
    ["held", "text-copper"],
    ["pending", "text-muted-foreground"],
    ["yanked", "text-muted-foreground"],
    ["blocked", "text-destructive"],
  ] as const)("gives %s the hue DESIGN.md assigns it", (state, klass) => {
    expect(mountState(state).classes()).toContain(klass);
  });

  /**
   * "Never colour alone — pattern, word and hue all carry it." The matrix is
   * hidden from assistive tech precisely because the word is not: a screen
   * reader must get the state as text, not as nine unlabelled boxes.
   */
  it("hides the matrix from assistive tech and states the word instead", () => {
    const w = mount(Resolution, { props: { state: "held", label: "Held" } });
    expect(w.find('[data-testid="resolution-matrix"]').attributes("aria-hidden")).toBe("true");
    expect(w.text()).toBe("Held");
  });

  /**
   * The hue is set once on the wrapper and the cells use `bg-current`, which is
   * what lets the mark sit inside a link or heading and take that ink. A cell
   * carrying its own colour would break that and silently reintroduce
   * colour-only state.
   */
  it("paints cells from currentColor rather than their own hue", () => {
    for (const cell of cells(mountState("cached"))) {
      expect(cell.classes()).toContain("bg-current");
    }
  });

  /**
   * The `resolve` animation is the list's job, not the mark's: DESIGN.md forbids
   * it "on load of unchanged content", and a component cannot tell whether it
   * just changed. If it ever appears here, it fires on every render.
   */
  it("carries no transition or animation of its own", () => {
    const html = mountState("cached").html();
    expect(html).not.toMatch(/transition|animate-|duration-/);
  });
});
