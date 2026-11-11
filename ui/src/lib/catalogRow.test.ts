import { describe, it, expect } from "vitest";
import type { ExploreEntryDto, UpstreamPackageDto } from "@/client/types.gen";
import {
  detailPath,
  matchLabel,
  noteKey,
  rowId,
  rowSnippet,
  rowState,
  type ExploreRow,
} from "./catalogRow";

const cached = (over: Partial<ExploreEntryDto> = {}): ExploreRow =>
  ({ kind: "cached", registry: "npm", name: "left-pad", state: "cached", ...over }) as ExploreRow;
const upstream = (over: Partial<UpstreamPackageDto> = {}): ExploreRow =>
  ({ kind: "upstream", registry: "npm", name: "chalk", ...over }) as ExploreRow;

describe("rowState", () => {
  it("is pending for an upstream hit, whatever it carries", () => {
    expect(rowState(upstream())).toBe("pending");
  });
  it("passes a known server state through", () => {
    expect(rowState(cached({ state: "blocked" }))).toBe("blocked");
  });
  it("falls back to pending for a state it does not know", () => {
    expect(rowState(cached({ state: "quarantined-ish" }))).toBe("pending");
  });
});

describe("noteKey", () => {
  it("names the rule for a refusal", () => {
    expect(noteKey(cached({ state: "blocked" }))).toBe("resolution.noteBlocked");
    expect(noteKey(cached({ state: "yanked" }))).toBe("resolution.noteYanked");
    expect(noteKey(cached({ state: "held" }))).toBe("resolution.noteHeld");
  });
  it("is absent for a state that is not a refusal", () => {
    expect(noteKey(cached())).toBeUndefined();
    expect(noteKey(upstream())).toBeUndefined();
  });
});

describe("matchLabel", () => {
  const t = (k: string) => `<${k}>`;
  it("labels a prose match only", () => {
    expect(matchLabel(t, cached({ matched_in: "readme" }))).toBe("<packageCatalog.matchedReadme>");
    expect(matchLabel(t, cached({ matched_in: "both" }))).toBe("<packageCatalog.matchedBoth>");
    expect(matchLabel(t, cached({ matched_in: "name" }))).toBeUndefined();
    expect(matchLabel(t, upstream())).toBeUndefined();
  });
});

describe("rowSnippet", () => {
  it("is the cached row's snippet, and nothing for an upstream hit", () => {
    expect(rowSnippet(cached({ snippet: "a fragment" }))).toBe("a fragment");
    expect(rowSnippet(cached({ snippet: null }))).toBeUndefined();
    expect(rowSnippet(upstream())).toBeUndefined();
  });
});

describe("rowId / detailPath", () => {
  it("keys a row by kind and identity", () => {
    expect(rowId(cached())).toBe("cached-npm/left-pad");
    expect(rowId(upstream())).toBe("upstream-npm/chalk");
  });
  it("leads both kinds to the package page", () => {
    expect(detailPath(upstream())).toBe(
      detailPath({ ...upstream(), kind: "cached" } as ExploreRow),
    );
  });
});
