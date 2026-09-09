import { describe, it, expect } from "vitest";
import { mount } from "@vue/test-utils";

import PackageHeaderCard from "./PackageHeaderCard.vue";

/**
 * The card every package page opens with: the coordinate's name, where it came
 * from, and three slots the pages fill with what only they know.
 *
 * The upstream row is the part worth pinning. A package served from a local
 * registry has no upstream, and a card that rendered an empty link there would
 * offer the reader a dead target where the honest answer is "there isn't one".
 */
describe("PackageHeaderCard", () => {
  it("shows the package name and links to its upstream", () => {
    const w = mount(PackageHeaderCard, {
      props: { name: "left-pad", upstreamUrl: "https://registry.npmjs.org/left-pad" },
    });
    expect(w.text()).toContain("left-pad");

    const link = w.get("a");
    expect(link.attributes("href")).toBe("https://registry.npmjs.org/left-pad");
    expect(link.text()).toBe("https://registry.npmjs.org/left-pad");
    // Opened in a new tab, so the reader does not lose the page they were on;
    // `noopener` because the target is a third-party origin.
    expect(link.attributes("target")).toBe("_blank");
    expect(link.attributes("rel")).toContain("noopener");
  });

  it("says a package has no upstream rather than offering a dead link", () => {
    const w = mount(PackageHeaderCard, { props: { name: "internal-lib", upstreamUrl: null } });
    expect(w.find("a").exists()).toBe(false);
    expect(w.text()).toContain("—");
  });

  it("renders the three slots the pages fill", () => {
    const w = mount(PackageHeaderCard, {
      props: { name: "left-pad", upstreamUrl: null },
      slots: {
        badges: "<span data-testid='badges'>yanked</span>",
        "before-upstream": "<span data-testid='before'>1.3.1</span>",
        default: "<span data-testid='default'>published 2026</span>",
      },
    });
    expect(w.find('[data-testid="badges"]').exists()).toBe(true);
    expect(w.find('[data-testid="before"]').exists()).toBe(true);
    expect(w.find('[data-testid="default"]').exists()).toBe(true);
  });
});
