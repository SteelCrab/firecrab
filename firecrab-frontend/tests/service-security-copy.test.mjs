import test from "node:test";
import assert from "node:assert/strict";
import { readingLines, severityRank } from "../src/lib/serviceSecurity.ts";

test("security guidance preserves all words in short reading lines", () => {
  const text =
    "Review the authentication policy before changing any service settings.\n<svg onload=alert(1)> remains plain text, never executable markup.";
  const lines = readingLines(text);
  assert.ok(lines.every((line) => line.split(/\s+/).length <= 8));
  assert.equal(lines.join(" "), text.split(/\s+/).join(" "));
});

test("security copy preserves versions and Korean sentences", () => {
  assert.deepEqual(
    readingLines(
      "Update libcrypto3 to 3.0.16-r0. 서비스를 다시 배포한다. 새 이미지를 검사한다.",
    ),
    [
      "Update libcrypto3 to 3.0.16-r0.",
      "서비스를 다시 배포한다.",
      "새 이미지를 검사한다.",
    ],
  );
  assert.deepEqual(readingLines("   "), []);
  assert.ok(severityRank("critical") < severityRank("high"));
  assert.ok(severityRank("high") < severityRank("medium"));
  assert.ok(severityRank("low") < severityRank(undefined));
});
