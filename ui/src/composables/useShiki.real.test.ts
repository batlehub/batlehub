import { describe, expect, it } from "vitest";
import { ensureLang, highlightInto, useShiki } from "./useShiki";

/** The real highlighter, unmocked (`useShiki.test.ts` mocks it): what a README fence turns into is the point of this file. */
describe("useShiki", () => {
  it.each([
    ["", null],
    ["  ", null],
    ["text", null],
    ["plaintext", null],
    ["brainfuck", null],
    ["yml", "yaml"],
    ["Bash", "bash"],
    ["js", "javascript"],
    ["sh", "shellscript"],
    ["rust", "rust"],
  ])("resolves the fence %j to %j", async (fence, lang) => {
    expect(await ensureLang(fence)).toBe(lang);
  });

  it("answers a lazily loaded language from its cache the second time", async () => {
    expect(await ensureLang("py")).toBe("python");
    expect(await ensureLang("python")).toBe("python");
  });

  it("paints tokens as DOM nodes, never as markup", async () => {
    const el = document.createElement("pre");
    const painted = await highlightInto(el, 'name = "<img src=x onerror=alert(1)>"\nx = 1', "toml");
    expect(painted).toBe(true);
    expect(el.querySelector("img")).toBeNull();
    expect(el.textContent).toBe('name = "<img src=x onerror=alert(1)>"\nx = 1');
    expect(el.querySelector("span")?.getAttribute("style")).toContain("--shiki-light");
  });

  it("leaves an unknown fence alone", async () => {
    const el = document.createElement("pre");
    el.textContent = "plain";
    expect(await highlightInto(el, "plain", "nope")).toBe(false);
    expect(el.textContent).toBe("plain");
  });

  it("highlights to HTML once ready, and answers empty for a language it lacks", async () => {
    const { highlight, ready } = useShiki();
    await ensureLang("bash"); // waits for nothing; the next line does
    await ensureLang("go");
    expect(ready.value).toBe(true);
    expect(highlight("echo hi", "bash")).toContain("shiki");
    expect(highlight("x", "not-loaded")).toBe("");
  });
});
