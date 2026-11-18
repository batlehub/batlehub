import { describe, expect, it } from "vitest";
import { mount } from "@vue/test-utils";
import { DateTime } from ".";

describe("DateTime (RFC 0050 §6.7)", () => {
  it("draws the one format, with the instant and a relative title", () => {
    const iso = new Date(2026, 10, 16, 17, 44).toISOString();
    const time = mount(DateTime, { props: { iso } }).get("time");
    expect(time.text()).toBe("2026-11-16 17:44");
    expect(time.attributes("datetime")).toBe(iso);
    expect(time.attributes("title")).toBeTruthy();
  });

  it("draws the fallback, and no <time>, for a missing date", () => {
    const w = mount(DateTime, { props: { iso: null } });
    expect(w.find("time").exists()).toBe(false);
    expect(w.text()).toBe("—");
  });
});
