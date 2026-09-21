import assert from "node:assert/strict";
import { test } from "node:test";

import { extractDescription } from "./page-description.mjs";

test("takes the first paragraph, not the heading", () => {
  assert.equal(
    extractDescription("# Installation\n\nBatleHub is a single binary.\n\nMore.\n"),
    "BatleHub is a single binary.",
  );
});

test("skips frontmatter, tables and rules — an RFC lands on its summary", () => {
  const rfc = [
    "---",
    "reference: true",
    "---",
    "",
    "# RFC 0031 — Ansible Galaxy",
    "",
    "| Field | Value |",
    "| --- | --- |",
    "| Status | Implemented |",
    "",
    "---",
    "",
    "## 1. Summary",
    "",
    "Ansible is the one automation tool in the roadmap with no adapter.",
  ].join("\n");
  assert.equal(
    extractDescription(rfc),
    "Ansible is the one automation tool in the roadmap with no adapter.",
  );
});

test("skips fenced code and strips inline markdown", () => {
  const src = "# X\n\n```toml\ntype = \"npm\"\n```\n\nPoint **npm** at [your registry](/guide) with `npm config`.\n";
  assert.equal(
    extractDescription(src),
    "Point npm at your registry with npm config.",
  );
});

test("clamps on a sentence, else on a word", () => {
  const long = `${"word ".repeat(40)}end.`;
  const out = extractDescription(`# X\n\n${long}\n`);
  assert.ok(out.length <= 161, out.length);
  assert.ok(out.endsWith("…"));
  const sentences = `${"a".repeat(100)}. ${"b".repeat(100)}.`;
  assert.equal(extractDescription(`# X\n\n${sentences}\n`), `${"a".repeat(100)}.`);
});

test("no prose at all is empty, not garbage", () => {
  assert.equal(extractDescription("---\nlayout: home\nhero:\n  name: X\n---\n"), "");
});
