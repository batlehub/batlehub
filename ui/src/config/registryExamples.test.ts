import { describe, expect, it } from "vitest";
import { packageExampleFor } from "./registryExamples";
import { EXAMPLE_COORDINATES } from "./registryTypes";

describe("packageExampleFor", () => {
  it("offers each ecosystem a coordinate of its own", () => {
    expect(packageExampleFor("cargo")).toContain("serde");
    expect(packageExampleFor("maven")).toContain("org.slf4j:slf4j-api");
    expect(packageExampleFor("goproxy")).toContain("github.com/");
  });

  // Access Check and the warm field read one table: the same package per type.
  it("agrees with the Access Check example for every type", () => {
    for (const [type, c] of Object.entries(EXAMPLE_COORDINATES)) {
      expect(packageExampleFor(type)).toContain(c.name);
    }
  });

  it("never offers another ecosystem's package to an unlisted type", () => {
    expect(packageExampleFor("brand-new-kind")).toBe("name, name@1.2.3");
    expect(packageExampleFor(undefined)).not.toContain("lodash");
  });
});
