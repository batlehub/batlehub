import { describe, it, expect } from "vitest";
import { mount } from "@vue/test-utils";
import { defineComponent, h } from "vue";
import Tabs from "./Tabs.vue";
import TabsList from "./TabsList.vue";
import TabsTrigger from "./TabsTrigger.vue";
import TabsContent from "./TabsContent.vue";

const TestTabs = defineComponent({
  components: { Tabs, TabsList, TabsTrigger, TabsContent },
  template: `
    <Tabs default-value="a">
      <TabsList>
        <TabsTrigger value="a">Tab A</TabsTrigger>
        <TabsTrigger value="b">Tab B</TabsTrigger>
      </TabsList>
      <TabsContent value="a">Content A</TabsContent>
      <TabsContent value="b">Content B</TabsContent>
    </Tabs>
  `,
});

describe("Tabs", () => {
  it("shows the default tab content and hides the other", () => {
    const wrapper = mount(TestTabs);
    expect(wrapper.text()).toContain("Content A");
    expect(wrapper.text()).not.toContain("Content B");
  });

  it("marks the default trigger as active and the other as inactive", () => {
    const wrapper = mount(TestTabs);
    const triggers = wrapper.findAll('[role="tab"]');
    expect(triggers).toHaveLength(2);
    expect(triggers[0].attributes("data-state")).toBe("active");
    expect(triggers[1].attributes("data-state")).toBe("inactive");
  });

  it("switches active content when a trigger is clicked", async () => {
    const wrapper = mount(TestTabs);
    const triggers = wrapper.findAll('[role="tab"]');

    // radix-vue activates tabs on mousedown (with the left button), not click.
    await triggers[1].trigger("mousedown", { button: 0 });

    expect(wrapper.text()).toContain("Content B");
    expect(wrapper.text()).not.toContain("Content A");
    expect(wrapper.findAll('[role="tab"]')[1].attributes("data-state")).toBe("active");
  });

  it("renders the TabsList with base classes", () => {
    const wrapper = mount(TestTabs);
    expect(wrapper.find('[role="tablist"]').classes()).toContain("inline-flex");
  });

  it("emits update:modelValue for a controlled v-model", async () => {
    const wrapper = mount(Tabs, {
      props: { modelValue: "a" },
      slots: {
        default: () =>
          h(TabsList, () => [
            h(TabsTrigger, { value: "a" }, () => "A"),
            h(TabsTrigger, { value: "b" }, () => "B"),
          ]),
      },
    });
    await wrapper.findAll('[role="tab"]')[1].trigger("mousedown", { button: 0 });
    expect(wrapper.emitted("update:modelValue")).toEqual([["b"]]);
  });
});
