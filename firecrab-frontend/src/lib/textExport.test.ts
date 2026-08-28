import assert from "node:assert/strict";
import { test } from "node:test";
import {
  prepareClipboardFallbackTextarea,
  shouldPreferAsyncClipboard,
} from "./textExport";

test("async clipboard write is only used in a secure context", () => {
  assert.equal(shouldPreferAsyncClipboard(true, true), true);
  assert.equal(shouldPreferAsyncClipboard(false, true), false);
  assert.equal(shouldPreferAsyncClipboard(true, false), false);
  assert.equal(shouldPreferAsyncClipboard(false, false), false);
});

test("fallback textarea stays in the viewport so iOS can copy", () => {
  const style: Record<string, string> = {};
  const area = {
    style,
    setAttribute() {},
  } as unknown as HTMLTextAreaElement;

  prepareClipboardFallbackTextarea(area);

  assert.equal(style.position, "fixed");
  assert.equal(style.left, "0");
  assert.equal(style.top, "0");
  assert.notEqual(style.left, "-9999px");
});
