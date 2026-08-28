import assert from "node:assert/strict";
import { test } from "node:test";
import {
  classifyTerminalGesture,
  linearSelect,
  linesFromPointerDelta,
  shouldPanTerminalPointer,
} from "./terminal";

test("touch and pen pan; mouse pans only when the device has no hover", () => {
  assert.equal(shouldPanTerminalPointer({ isPrimary: true, pointerType: "touch" }, false), true);
  assert.equal(shouldPanTerminalPointer({ isPrimary: true, pointerType: "pen" }, false), true);
  assert.equal(shouldPanTerminalPointer({ isPrimary: true, pointerType: "mouse" }, false), false);
  assert.equal(shouldPanTerminalPointer({ isPrimary: true, pointerType: "mouse" }, true), true);
  assert.equal(shouldPanTerminalPointer({ isPrimary: false, pointerType: "touch" }, false), false);
});

test("pointer delta keeps a sub-cell remainder so slow drags still accumulate", () => {
  const whole = linesFromPointerDelta(20, 10, 0);
  assert.equal(whole.lines, -2);
  assert.ok(whole.acc === 0);

  const partial = linesFromPointerDelta(8, 10, 0);
  assert.equal(partial.lines, 0);
  assert.ok(partial.acc < 0);

  const combined = linesFromPointerDelta(8, 10, partial.acc);
  assert.equal(combined.lines, -1);
});

test("a quick drag pans; a still hold then drag stays hold", () => {
  assert.equal(classifyTerminalGesture(80, 24, "pending"), "pan");
  assert.equal(classifyTerminalGesture(80, 2, "pending"), "pending");
  assert.equal(classifyTerminalGesture(520, 2, "pending"), "hold");
  assert.equal(classifyTerminalGesture(600, 40, "hold"), "hold");
  assert.equal(classifyTerminalGesture(600, 40, "pan"), "pan");
});

test("linearSelect spans cells across rows", () => {
  assert.deepEqual(linearSelect(10, 2, 3, 5, 3), { column: 2, row: 3, length: 4 });
  assert.deepEqual(linearSelect(10, 8, 1, 1, 2), { column: 8, row: 1, length: 4 });
});
