import { describe, it, expect, vi } from "vitest";
import { mount } from "@vue/test-utils";
import { nextTick } from "vue";

const held = { verbs: [] as string[] };
vi.mock("@/composables/useAuth", () => ({
  useAuth: () => ({ can: (verb: string) => held.verbs.includes(verb) }),
}));

import SecurityDashboard from "./SecurityDashboard.vue";

const stubs = {
  HoldsPanel: {
    name: "HoldsPanel",
    props: ["canOpenFindings", "findingsFor"],
    template: "<section data-testid='holds' />",
  },
  FlagsPanel: { template: "<section data-testid='flags' />" },
  ExposurePanel: { template: "<section data-testid='exposure' />" },
  FindingsPanel: { template: "<section data-testid='findings' />" },
};

function sections(verbs: string[]) {
  held.verbs = verbs;
  const w = mount(SecurityDashboard, { global: { stubs } });
  return ["holds", "flags", "exposure"].filter((s) => w.find(`[data-testid='${s}']`).exists());
}

/** RFC 0042 §4.3: a section per verb, and nothing the viewer cannot use. */
describe("SecurityDashboard", () => {
  it("draws only the sections whose verb is held", () => {
    expect(sections(["quarantine:read"])).toEqual(["holds"]);
    expect(sections(["flags:read", "audit:read"])).toEqual(["flags", "exposure"]);
    expect(sections(["quarantine:read", "flags:read", "audit:read"])).toEqual([
      "holds",
      "flags",
      "exposure",
    ]);
  });

  it("opens a registry's findings from the holds panel only with findings:read", async () => {
    held.verbs = ["quarantine:read", "findings:read"];
    const w = mount(SecurityDashboard, { global: { stubs } });
    const holds = w.findComponent({ name: "HoldsPanel" });
    expect(holds.props("canOpenFindings")).toBe(true);
    holds.vm.$emit("openFindings", "npm1");
    await nextTick();
    expect(w.find("[data-testid='findings']").exists()).toBe(true);

    held.verbs = ["quarantine:read"];
    const narrow = mount(SecurityDashboard, { global: { stubs } });
    expect(narrow.findComponent({ name: "HoldsPanel" }).props("canOpenFindings")).toBe(false);
  });
});
