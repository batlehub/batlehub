import { describe, expect, it } from "vitest";

import { miseBlockGuess } from "./miseBlock";

describe("miseBlockGuess", () => {
  it("takes the repository name and strips a `<repo>-` tag prefix", () => {
    expect(miseBlockGuess("jqlang/jq", "jq-1.8.2")).toEqual({ tool: "jq", version: "1.8.2" });
  });

  it("strips a leading `v` before a digit", () => {
    expect(miseBlockGuess("cli/cli", "v2.60.0")).toEqual({ tool: "cli", version: "2.60.0" });
  });

  it("keeps a tag it cannot read, and the last segment of a nested project", () => {
    expect(miseBlockGuess("group/sub/tool", "release-3")).toEqual({
      tool: "tool",
      version: "release-3",
    });
    expect(miseBlockGuess("a/vault", "vault")).toEqual({ tool: "vault", version: "vault" });
  });
});
