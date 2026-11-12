import { describe, expect, it } from "vitest";
import { locateRegistry } from "./configLocate";

const TOML = [
  "[server]",
  'name = "npm"',
  "",
  "[[registries]]",
  'name = "cargo"',
  'type = "cargo"',
  "",
  "[[registries]]",
  'type = "npm"  # the public one',
  "name = 'npm'   # quoted the other way",
  "",
  "[registries.rbac]",
  'name = "not-a-registry"',
].join("\n");

describe("locateRegistry", () => {
  it("puts the caret at the end of the registry's name line", () => {
    const at = locateRegistry(TOML, "npm");
    expect(at?.line).toBe(9);
    expect(TOML.slice(0, at?.offset).endsWith("name = 'npm'   # quoted the other way")).toBe(true);
  });

  it("ignores a name key outside a [[registries]] table", () => {
    expect(locateRegistry('[server]\nname = "npm"\n', "npm")).toBeNull();
  });

  it("answers nothing for a registry the text does not define (another layer)", () => {
    expect(locateRegistry(TOML, "pypi")).toBeNull();
  });

  it("counts CRLF line endings in the offset", () => {
    const crlf = '[[registries]]\r\nname = "go"\r\n';
    expect(locateRegistry(crlf, "go")).toEqual({ offset: crlf.indexOf("\r\n", 16), line: 1 });
  });

  it("matches the whole name, not a prefix", () => {
    expect(locateRegistry(TOML, "np")).toBeNull();
  });
});
