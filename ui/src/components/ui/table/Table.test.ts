import { describe, it, expect } from "vitest";
import { mount } from "@vue/test-utils";
import Table from "./Table.vue";
import TableHeader from "./TableHeader.vue";
import TableHead from "./TableHead.vue";
import TableBody from "./TableBody.vue";
import TableRow from "./TableRow.vue";
import TableCell from "./TableCell.vue";
import TableCaption from "./TableCaption.vue";

describe("Table", () => {
  it("renders a wrapped table element with base classes", () => {
    const wrapper = mount(Table, { slots: { default: "rows" } });
    expect(wrapper.find("table").exists()).toBe(true);
    expect(wrapper.find("table").classes()).toContain("w-full");
    expect(wrapper.find("table").classes()).toContain("text-sm");
  });

  it("merges a custom class onto the table element", () => {
    const wrapper = mount(Table, { props: { class: "my-table" } });
    expect(wrapper.find("table").classes()).toContain("my-table");
  });
});

describe("TableHeader", () => {
  it("renders slot content in a thead", () => {
    const wrapper = mount(TableHeader, { slots: { default: "head" } });
    expect(wrapper.element.tagName).toBe("THEAD");
    expect(wrapper.text()).toBe("head");
  });
});

describe("TableHead", () => {
  it("renders slot content in a th with base classes", () => {
    const wrapper = mount(TableHead, { slots: { default: "Name" } });
    expect(wrapper.element.tagName).toBe("TH");
    expect(wrapper.text()).toBe("Name");
    expect(wrapper.classes()).toContain("text-left");
    expect(wrapper.classes()).toContain("uppercase");
  });

  it("merges a custom class", () => {
    const wrapper = mount(TableHead, { props: { class: "my-head" }, slots: { default: "x" } });
    expect(wrapper.classes()).toContain("my-head");
  });
});

describe("TableBody", () => {
  it("renders slot content in a tbody", () => {
    const wrapper = mount(TableBody, { slots: { default: "body" } });
    expect(wrapper.element.tagName).toBe("TBODY");
    expect(wrapper.text()).toBe("body");
  });
});

describe("TableRow", () => {
  it("renders slot content in a tr with base classes", () => {
    const wrapper = mount(TableRow, { slots: { default: "row" } });
    expect(wrapper.element.tagName).toBe("TR");
    expect(wrapper.classes()).toContain("border-b");
  });

  it("merges a custom class", () => {
    const wrapper = mount(TableRow, { props: { class: "my-row" }, slots: { default: "x" } });
    expect(wrapper.classes()).toContain("my-row");
  });

  // The hover fill is the neutral badge's fill: an engaged row repaints its chips.
  it("keeps neutral badges visible on a hovered or selected row", () => {
    const wrapper = mount(TableRow, { slots: { default: "x" } });
    expect(wrapper.classes()).toContain("hover:[&_.bg-muted]:bg-background");
    expect(wrapper.classes()).toContain("data-[state=selected]:[&_.bg-muted]:bg-background");
  });
});

describe("TableCell", () => {
  it("renders slot content in a td with base classes", () => {
    const wrapper = mount(TableCell, { slots: { default: "cell" } });
    expect(wrapper.element.tagName).toBe("TD");
    expect(wrapper.text()).toBe("cell");
    expect(wrapper.classes()).toContain("p-4");
    expect(wrapper.classes()).toContain("align-middle");
  });

  it("merges a custom class", () => {
    const wrapper = mount(TableCell, { props: { class: "my-cell" }, slots: { default: "x" } });
    expect(wrapper.classes()).toContain("my-cell");
  });
});

describe("TableCaption", () => {
  it("renders slot content in a caption with base classes", () => {
    const wrapper = mount(TableCaption, { slots: { default: "caption" } });
    expect(wrapper.element.tagName).toBe("CAPTION");
    expect(wrapper.text()).toBe("caption");
    expect(wrapper.classes()).toContain("text-muted-foreground");
  });
});

describe("Table stacked (RFC 0050 §6.7)", () => {
  it("stacks rows below md only when asked", () => {
    const flat = mount(Table, { slots: { default: "<tbody />" } }).get("table");
    expect(flat.classes()).not.toContain("max-md:[&_tr]:block");
    const stacked = mount(Table, { props: { stacked: true }, slots: { default: "<tbody />" } });
    expect(stacked.get("table").classes()).toContain("max-md:[&_tr]:block");
    expect(stacked.get("table").classes()).toContain("max-md:[&_thead]:sr-only");
  });
});
