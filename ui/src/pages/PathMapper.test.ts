import { describe, it, expect, vi, beforeEach } from "vitest";
import { mount, flushPromises } from "@vue/test-utils";

const { listRegistriesMock } = vi.hoisted(() => ({ listRegistriesMock: vi.fn() }));
vi.mock("@/client/sdk.gen", () => ({ listRegistries: listRegistriesMock }));

import PathMapper from "./PathMapper.vue";
import { REGISTRY_PATH_TYPES } from "@/config/registryPathFields";

const stubs = {
  RegistryPathForm: {
    name: "RegistryPathForm",
    props: ["registryName", "values", "typeDef", "registries"],
    template: "<div/>",
  },
  RegistryPathResults: {
    name: "RegistryPathResults",
    props: ["paths", "baseUrl"],
    template: "<div/>",
  },
};

async function mountPage(registries: Array<{ name: string; type: string }> = []) {
  listRegistriesMock.mockResolvedValue({ data: registries });
  const w = mount(PathMapper, { global: { stubs } });
  await flushPromises();
  return w;
}

const form = (w: Awaited<ReturnType<typeof mountPage>>) =>
  w.findComponent({ name: "RegistryPathForm" });

beforeEach(() => listRegistriesMock.mockReset());

/**
 * `/tools/url-mapper`: paste the upstream URL you already have, get the one to
 * put in your tool.
 *
 * The paste box is the whole point of the page — the alternative is knowing
 * which of twenty registry types you are holding and which fields it takes —
 * so the parser rows are what these tests are about.
 */
describe("PathMapper", () => {
  it("offers every configured registry type, grouped", async () => {
    const w = await mountPage();
    const groups = w.findAll("optgroup");
    expect(groups.length).toBeGreaterThan(1);
    expect(w.findAll("option")).toHaveLength(REGISTRY_PATH_TYPES.length);
  });

  it("names the registry this server actually serves for a type", async () => {
    // The default is the type id, which is a guess; the server's own name for
    // the registry is what the generated path has to carry.
    const w = await mountPage([{ name: "gh-mirror", type: "github" }]);
    expect(form(w).props("registryName")).toBe("gh-mirror");
    expect(form(w).props("registries")).toEqual([{ name: "gh-mirror", type: "github" }]);
  });

  it("keeps the type id when the server serves no registry of that type", async () => {
    const w = await mountPage([{ name: "npm-mirror", type: "npm" }]);
    expect(form(w).props("registryName")).toBe("github");
  });

  it("reads a pasted GitHub release URL into the right type and fields", async () => {
    const w = await mountPage();
    await w.get("#paste-url").setValue("https://github.com/cli/cli/releases/tag/v2.60.0");
    await flushPromises();

    expect(form(w).props("typeDef")).toMatchObject({ id: "github" });
    expect(form(w).props("values")).toMatchObject({ owner: "cli", repo: "cli", ref: "v2.60.0" });
  });

  it("switches type when the pasted URL belongs to another registry", async () => {
    const w = await mountPage();
    await w.get("#paste-url").setValue("https://registry.npmjs.org/left-pad/1.3.0");
    await flushPromises();

    expect(form(w).props("typeDef")).toMatchObject({ id: "npm" });
    expect(form(w).props("values")).toMatchObject({ package: "left-pad", version: "1.3.0" });
  });

  /**
   * `notnpmjs.org` is not npm. The host check is a suffix match on purpose, and
   * a look-alike that parsed would send a user's request to a path built for
   * the wrong registry.
   */
  it("ignores a look-alike host and anything that is not a URL", async () => {
    const w = await mountPage();
    const before = form(w).props("typeDef");

    await w.get("#paste-url").setValue("https://evil-npmjs.org.attacker.test/package/left-pad");
    await flushPromises();
    expect(form(w).props("typeDef")).toEqual(before);

    await w.get("#paste-url").setValue("not a url at all");
    await flushPromises();
    expect(form(w).props("typeDef")).toEqual(before);
  });

  it("hands the built paths, under this server's registry name, to the results panel", async () => {
    const w = await mountPage([{ name: "gh-mirror", type: "github" }]);
    // The coordinate has to be filled: a type with no owner/repo builds nothing,
    // which is what the empty panel means rather than a failure.
    await w.get("#paste-url").setValue("https://github.com/cli/cli/releases/tag/v2.60.0");
    await flushPromises();
    const paths = w.findComponent({ name: "RegistryPathResults" }).props("paths") as unknown[];
    expect(Array.isArray(paths)).toBe(true);
    expect(paths.length).toBeGreaterThan(0);
    expect(JSON.stringify(paths)).toContain("gh-mirror");
  });
});
