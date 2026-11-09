// Find and replace in rows (src/replace.ts). Run: npm test
import assert from "node:assert/strict";
import { test } from "node:test";
import { replaceText, rowChanges } from "../src/replace.ts";

const columns = [
  { name: "id", type: "text" },
  { name: "nome", type: "varchar(20)" },
  { name: "n", type: "integer" },
  { name: "obs", type: "TEXT" },
];

test("replaces in text columns, ignoring case", () => {
  assert.deepEqual(rowChanges(columns, ["x", "Ana ANA", "2", null], ["id"], "ana", "Bia"), [{ column: "nome", value: "Bia Bia" }]);
});

test("leaves keys and numbers alone", () => {
  assert.deepEqual(rowChanges(columns, ["a2", "b", "2", "c"], ["id"], "2", "3"), []);
});

test("an empty search changes nothing", () => {
  assert.deepEqual(rowChanges(columns, ["a", "b", "1", "c"], [], "", "x"), []);
});

test("the search and the replacement are plain text", () => {
  assert.equal(replaceText("1.5 e 1x5", "1.5", "$&"), "$& e 1x5");
});
