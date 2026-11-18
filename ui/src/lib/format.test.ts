import { describe, it, expect } from "vitest";
import { formatBytes, formatDate, formatRelative, formatCount } from "./format";

describe("formatBytes", () => {
  it("formats bytes below 1024 without decimals", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(1023)).toBe("1023 B");
  });

  it("formats kibibytes/mebibytes/gibibytes with one decimal", () => {
    expect(formatBytes(1024)).toBe("1.0 KiB");
    expect(formatBytes(1_048_576)).toBe("1.0 MiB");
    expect(formatBytes(1_073_741_824)).toBe("1.0 GiB");
  });

  it("returns the fallback for null/undefined", () => {
    expect(formatBytes(null)).toBe("—");
    expect(formatBytes(undefined)).toBe("—");
    expect(formatBytes(null, { fallback: "n/a" })).toBe("n/a");
  });
});

describe("formatDate", () => {
  it("formats a valid ISO date", () => {
    expect(formatDate("2026-01-02T00:00:00Z")).toBe(
      new Date("2026-01-02T00:00:00Z").toLocaleString("en", {
        dateStyle: "medium",
        timeStyle: "short",
      }),
    );
  });

  it("returns the fallback for null/undefined/invalid", () => {
    expect(formatDate(null)).toBe("—");
    expect(formatDate(undefined)).toBe("—");
    expect(formatDate("not-a-date")).toBe("—");
  });
});

describe("formatRelative", () => {
  it("returns 'Just now' for very recent timestamps", () => {
    expect(formatRelative(new Date().toISOString())).toBe("Just now");
  });

  it("returns minutes/hours/days ago for older timestamps", () => {
    expect(formatRelative(new Date(Date.now() - 5 * 60_000).toISOString())).toBe("5m ago");
    expect(formatRelative(new Date(Date.now() - 3 * 3_600_000).toISOString())).toBe("3h ago");
    expect(formatRelative(new Date(Date.now() - 2 * 86_400_000).toISOString())).toBe("2d ago");
  });

  it("returns the fallback for null/undefined", () => {
    expect(formatRelative(null)).toBe("Never");
    expect(formatRelative(undefined, { fallback: "n/a" })).toBe("n/a");
  });
});

describe("formatCount", () => {
  it("formats with locale thousands separators", () => {
    expect(formatCount(1234)).toBe((1234).toLocaleString());
  });

  it("returns the fallback for null/undefined", () => {
    expect(formatCount(null)).toBe("—");
    expect(formatCount(undefined, { fallback: "n/a" })).toBe("n/a");
  });
});
