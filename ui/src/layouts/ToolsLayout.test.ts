import { describe, it, expect } from "vitest";
import { mount } from "@vue/test-utils";

import ToolsLayout from "./ToolsLayout.vue";
import { TOOLS_TABS } from "@/config/navigation";

/**
 * `/tools` is the diagnostics hub: the same shell as `/me`, with a fixed tab
 * set. Its whole job is handing `TOOLS_TABS` to `HubLayout`, so that is what is
 * asserted — a hub that titled itself and then shipped someone else's tabs
 * would render, and be wrong.
 */
describe("ToolsLayout", () => {
  it("renders the diagnostics tabs under a titled hub", () => {
    const w = mount(ToolsLayout, {
      global: {
        stubs: {
          HubLayout: {
            name: "HubLayout",
            props: ["title", "tabs", "description"],
            template: "<div/>",
          },
        },
      },
    });
    const hub = w.findComponent({ name: "HubLayout" });
    expect(hub.props("tabs")).toEqual(TOOLS_TABS);
    expect(hub.props("title")).toBeTruthy();
    expect(hub.props("description")).toBeTruthy();
  });
});
