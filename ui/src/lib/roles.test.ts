import { describe, it, expect } from "vitest";
import { roleLabel } from "./roles";

describe("roleLabel", () => {
  const t = (k: string) => `<${k}>`;
  it("names each known role through the catalogue", () => {
    expect(roleLabel(t, "admin")).toBe("<common.admin>");
    expect(roleLabel(t, "user")).toBe("<common.user>");
    expect(roleLabel(t, "anonymous")).toBe("<common.roleAnonymous>");
    expect(roleLabel(t, undefined)).toBe("<common.roleAnonymous>");
  });
  it("passes an unknown role through rather than hiding it", () => {
    expect(roleLabel(t, "auditor")).toBe("auditor");
  });
});
