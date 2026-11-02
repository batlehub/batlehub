import { describe, it, expect } from "vitest";
import { DOCS_URL, docsUrl, reasonCodeDocsUrl } from "./config";

describe("docsUrl", () => {
  it("joins the path to the docs site without doubling the slash", () => {
    expect(DOCS_URL.endsWith("/")).toBe(true);
    expect(docsUrl("/use/cli")).toBe(`${DOCS_URL}use/cli`);
  });

  it("serves the French tree to a French reader", () => {
    expect(docsUrl("use/cli", "fr-FR")).toBe(`${DOCS_URL}fr/use/cli`);
    expect(docsUrl("use/cli", "en")).toBe(`${DOCS_URL}use/cli`);
  });
});

describe("reasonCodeDocsUrl", () => {
  it("links a wire code to its section anchor", () => {
    expect(reasonCodeDocsUrl("MIN_AGE_NOT_MET")).toBe(
      `${DOCS_URL}use/refused-downloads#min-age-not-met`,
    );
  });
});
