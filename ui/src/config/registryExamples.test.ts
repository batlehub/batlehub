import { describe, expect, it } from "vitest";
import { packageExampleFor } from "./registryExamples";

describe("packageExampleFor", () => {
  it("offers each ecosystem a coordinate of its own", () => {
    expect(packageExampleFor("cargo")).toContain("serde");
    expect(packageExampleFor("maven")).toContain("org.slf4j:slf4j-api");
    expect(packageExampleFor("goproxy")).toContain("github.com/");
  });

  it("never offers another ecosystem's package to an unlisted type", () => {
    expect(packageExampleFor("brand-new-kind")).toBe("name, name@1.2.3");
    expect(packageExampleFor(undefined)).not.toContain("lodash");
  });
});
