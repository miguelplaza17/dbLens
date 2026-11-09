// Where a dragged sidebar row lands (src/native/dragSpots.ts). Run: npm test
import assert from "node:assert/strict";
import { test } from "node:test";
import { resolveTarget, restY, type Snapshot } from "../src/native/dragSpots.ts";

// 28 px rows, from y = 40:
//   header 0..28
//   c1   (top level)         40..68
//   A    (open folder)       68..96
//     a1 (in A)              96..124
//     a2 (in A)             124..152
//     S  (subfolder of A)   152..180
//   B    (closed folder)    180..208
//   C    (closed folder)    208..236
const snap: Snapshot = {
  header: { top: 0, bottom: 28 },
  rows: [
    { kind: "connection", id: "c1", folder: "", top: 40, bottom: 68 },
    { kind: "folder", id: "A", parent: "", open: true, top: 68, bottom: 96 },
    { kind: "connection", id: "a1", folder: "A", top: 96, bottom: 124 },
    { kind: "connection", id: "a2", folder: "A", top: 124, bottom: 152 },
    { kind: "folder", id: "S", parent: "A", open: false, top: 152, bottom: 180 },
    { kind: "folder", id: "B", parent: "", open: false, top: 180, bottom: 208 },
    { kind: "folder", id: "C", parent: "", open: false, top: 208, bottom: 236 },
  ],
};

const spot = (folder: string, before: string | null = null, row: string | null = null) => ({ folder, before, row });

test("a connection goes before the row when over its upper half, before the next one over the lower half", () => {
  assert.deepEqual(resolveTarget(snap, 100, false)?.spot, spot("A", null, "a1"));
  assert.deepEqual(resolveTarget(snap, 120, false)?.spot, spot("A", null, "a2"));
  // After the last connection of a folder: the end of that folder's connections.
  assert.deepEqual(resolveTarget(snap, 148, false)?.spot, spot("A", null, null));
  assert.deepEqual(resolveTarget(snap, 60, false)?.spot, spot("", null, null));
  assert.equal(resolveTarget(snap, 100, false)?.into, null, "between rows lights no folder");
});

test("a connection over a folder goes inside it, or after the connections above it at the top edge", () => {
  const middle = resolveTarget(snap, 194, false);
  assert.deepEqual(middle?.spot, spot("B"));
  assert.equal(middle?.into, "B");
  assert.deepEqual(resolveTarget(snap, 181, false)?.spot, spot("", null, null), "top edge: end of the top level's connections");
  assert.deepEqual(resolveTarget(snap, 153, false)?.spot, spot("A", null, null), "top edge of a subfolder: end of its parent's connections");
});

test("a folder is put in order at its own level by the top and bottom quarter of the folder row", () => {
  assert.deepEqual(resolveTarget(snap, 181, true)?.spot, spot("", "B"), "top quarter: in front");
  assert.deepEqual(resolveTarget(snap, 205, true)?.spot, spot("", "C"), "bottom quarter of B: in front of C");
  assert.deepEqual(resolveTarget(snap, 233, true)?.spot, spot("", null), "bottom quarter of the last one: last");
  const inside = resolveTarget(snap, 194, true);
  assert.deepEqual(inside?.spot, spot("B"));
  assert.equal(inside?.into, "B");
  assert.deepEqual(resolveTarget(snap, 153, true)?.spot, spot("A", "S"), "in front of a subfolder");
});

test("the bottom of an open folder means inside it, not after its contents", () => {
  const target = resolveTarget(snap, 94, true);
  assert.deepEqual(target?.spot, spot("A"));
  assert.equal(target?.into, "A");
});

test("a folder over a connection goes among that folder's subfolders", () => {
  assert.deepEqual(resolveTarget(snap, 110, true)?.spot, spot("A", "S"));
  assert.deepEqual(resolveTarget(snap, 50, true)?.spot, spot("", "A"), "a top-level connection: before the first folder");
});

test("the header and the empty part below the rows mean the top level", () => {
  const header = resolveTarget(snap, 10, false);
  assert.deepEqual(header?.spot, spot(""));
  assert.equal(header?.into, "");
  assert.deepEqual(resolveTarget(snap, 400, true)?.spot, spot("", null), "after the last row: last at the top level");
  assert.deepEqual(resolveTarget(snap, 400, false)?.spot, spot("", null));
});

test("spots the drag can't take give nothing", () => {
  const notB = (s: { folder: string; before: string | null }) => s.before !== "B" && s.folder !== "B";
  assert.equal(resolveTarget(snap, 194, true, notB), null);
  assert.deepEqual(resolveTarget(snap, 205, true, notB)?.spot, spot("", "C"));
});

test("the pointer is mapped onto the layout without the open gap", () => {
  assert.equal(restY(100, null), 100);
  const gap = { top: 120, height: 30 };
  assert.equal(restY(110, gap), 110, "above the gap: unchanged");
  assert.equal(restY(135, gap), 120, "inside the gap: its top edge");
  assert.equal(restY(180, gap), 150, "below the gap: the rows have moved down by its height");
});
