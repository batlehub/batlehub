import { describe, it, expect } from "vitest";
import {
  variantFromMap,
  REGISTRY_TYPE_VARIANTS,
  sourceVariant,
  firewallVariant,
  severityVariant,
  eventBadgeVariant,
} from "./badge-variants";

describe("variantFromMap", () => {
  it("returns the mapped variant", () => {
    expect(variantFromMap("npm", REGISTRY_TYPE_VARIANTS)).toBe("default");
  });

  it("falls back when unmapped", () => {
    expect(variantFromMap("unknown", REGISTRY_TYPE_VARIANTS)).toBe("outline");
    expect(variantFromMap("unknown", REGISTRY_TYPE_VARIANTS, "secondary")).toBe("secondary");
  });
});

describe("sourceVariant", () => {
  it("maps known sources", () => {
    expect(sourceVariant("local")).toBe("secondary");
    expect(sourceVariant("both")).toBe("default");
    expect(sourceVariant("upstream")).toBe("outline");
  });
});

describe("firewallVariant", () => {
  it("maps status strings", () => {
    expect(firewallVariant(undefined)).toBe("outline");
    expect(firewallVariant("blocked")).toBe("destructive");
    expect(firewallVariant("yanked")).toBe("secondary");
    expect(firewallVariant("clear")).toBe("outline");
  });
});

describe("severityVariant", () => {
  it("maps severities", () => {
    expect(severityVariant("critical")).toBe("destructive");
    expect(severityVariant("high")).toBe("destructive");
    expect(severityVariant("medium")).toBe("copper");
    expect(severityVariant("low")).toBe("secondary");
  });

  /**
   * `default` and `destructive` both resolve to `--accent` — `assets/index.css`
   * sets `--destructive: var(--accent)`. So `medium` returning `default`
   * painted it the identical crimson as `critical` and `high`, and the three
   * severities were distinguishable only by reading their own text.
   *
   * Asserted as a property of the *set* rather than value by value: a fourth
   * severity mapped to `default` tomorrow would pass the test above and
   * re-introduce exactly this.
   */
  it("does not paint a non-critical severity in the refusal hue", () => {
    const crimson = new Set(["default", "destructive"]);
    expect(crimson.has(severityVariant("medium"))).toBe(false);
    expect(crimson.has(severityVariant("low"))).toBe(false);
  });
});

describe("eventBadgeVariant", () => {
  it("maps notification event types", () => {
    expect(eventBadgeVariant("package_published")).toBe("default");
    expect(eventBadgeVariant("package_yanked")).toBe("destructive");
    expect(eventBadgeVariant("package_unyanked")).toBe("secondary");
    // RFC 0014 §4.5
    expect(eventBadgeVariant("package_disappeared_upstream")).toBe("destructive");
    expect(eventBadgeVariant("package_reappeared_upstream")).toBe("secondary");
    expect(eventBadgeVariant("upstream_unreachable")).toBe("copper");
    expect(eventBadgeVariant("other")).toBe("outline");
  });
});
