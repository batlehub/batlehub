import { describe, expect, it } from "vitest";
import { leadSentence, sentenceStart, withoutDocRefs } from "./plainCopy";

describe("withoutDocRefs", () => {
  it("drops a parenthesised RFC citation and its section", () => {
    expect(withoutDocRefs("Add a token (RFC 0019 §4.2). Then reload.")).toBe(
      "Add a token. Then reload.",
    );
  });

  it("drops a citation carrying a suffix or a second reference", () => {
    expect(withoutDocRefs("Shadowed (RFC 0045-bis §3, §4) here.")).toBe("Shadowed here.");
  });

  it("leaves text without a citation untouched", () => {
    expect(withoutDocRefs("No upstream_auth is configured.")).toBe(
      "No upstream_auth is configured.",
    );
  });
});

describe("leadSentence", () => {
  it("splits the claim from its explanation", () => {
    expect(leadSentence("Registry 'github' serves no raw content. Add enabled = true.")).toEqual({
      lead: "Registry 'github' serves no raw content.",
      rest: "Add enabled = true.",
    });
  });

  it("keeps a single sentence whole", () => {
    expect(leadSentence("Nothing to add.")).toEqual({ lead: "Nothing to add.", rest: "" });
  });

  it("does not split after an abbreviation", () => {
    expect(leadSentence("Use a mirror, e.g. Nexus or Artifactory. Then reload.")).toEqual({
      lead: "Use a mirror, e.g. Nexus or Artifactory.",
      rest: "Then reload.",
    });
  });

  it("does not split inside a dotted key", () => {
    expect(leadSentence("server.trusted_proxies is unset. Set it.")).toEqual({
      lead: "server.trusted_proxies is unset.",
      rest: "Set it.",
    });
  });
});

describe("sentenceStart", () => {
  it("capitalises a prose opening", () => {
    expect(sentenceStart("no upstream is set.")).toBe("No upstream is set.");
  });

  it("leaves an opening identifier as the server spelled it", () => {
    for (const text of [
      "overlay = true shadows the base.",
      "on_confirmed is ignored here.",
      "github:jdx/mise has no release.",
      "server.trusted_proxies is empty.",
    ]) {
      expect(sentenceStart(text)).toBe(text);
    }
  });
});
