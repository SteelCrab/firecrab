import assert from "node:assert/strict";
import { test } from "node:test";
import {
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
