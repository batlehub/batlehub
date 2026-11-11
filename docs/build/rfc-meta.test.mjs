import assert from "node:assert/strict";
import { test } from "node:test";

import { bannerNote } from "./rfc-meta.mjs";

test("keeps the first sentence and drops the rest", () => {
  assert.equal(
    bannerNote("every phase has landed, see §13.1. §12 carries no open question."),
    "every phase has landed, see §13.1.",
  );
});

test("stops at a semicolon and removes markdown markers", () => {
  assert.equal(
    bannerNote("all phases landed; see the *implementation* notes in `§13`"),
    "all phases landed",
  );
});

test("caps a long first sentence with an ellipsis", () => {
  const out = bannerNote("a ".repeat(200), 20);
  assert.equal(out.length, 20);
  assert.ok(out.endsWith("…"));
});

test("an empty note stays empty, so the banner shows the state alone", () => {
  assert.equal(bannerNote(""), "");
});
