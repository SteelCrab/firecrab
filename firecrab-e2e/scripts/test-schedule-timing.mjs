import assert from "node:assert/strict";
import test from "node:test";
import { buildTiming, timingDraft, timingLabel } from "../../firecrab-frontend/src/components/schedules/timing.ts";

test("simple repeats produce the existing API contract", () => {
  const draft = { ...timingDraft(), timeZone: "Asia/Seoul", time: "18:30" };
  assert.deepEqual(buildTiming(draft), { kind: "cron", expression: "30 18 * * Mon-Fri", timeZone: "Asia/Seoul" });
  assert.deepEqual(buildTiming({ ...draft, repeat: "daily" }), { kind: "cron", expression: "30 18 * * *", timeZone: "Asia/Seoul" });
  assert.deepEqual(buildTiming({ ...draft, repeat: "weekly", days: ["Fri", "Mon", "Wed"] }), { kind: "cron", expression: "30 18 * * Mon,Wed,Fri", timeZone: "Asia/Seoul" });
  assert.equal(buildTiming({ ...draft, repeat: "weekly", days: [] }), null);
  assert.equal(buildTiming({ ...draft, time: "25:00" }), null);
});

test("supported presets round trip and uncommon cron expressions stay custom", () => {
  for (const expression of ["0 9 * * Mon-Fri", "15 23 * * *", "30 8 * * Mon,Wed,Fri", "*/15 9-18 * * Mon-Fri", "0 9 1 * *", "0 9 * * 1-5"]) {
    const timing = { kind: "cron", expression, timeZone: "America/New_York" };
    assert.deepEqual(buildTiming(timingDraft(timing)), timing);
  }
  assert.equal(timingLabel({ kind: "cron", expression: "0 9 * * Mon-Fri", timeZone: "Asia/Seoul" }, "ko"), "평일 09:00");
});

test("one-time inputs use the displayed local zone and an explicit UTC instant", () => {
  const previous = process.env.TZ;
  try {
    process.env.TZ = "Asia/Seoul";
    const draft = { ...timingDraft(), repeat: "once", date: "2030-01-01", time: "18:00" };
    assert.deepEqual(buildTiming(draft), { kind: "once", at: "2030-01-01T09:00:00.000Z" });
    assert.equal(buildTiming({ ...draft, date: "2030-02-30" }), null);
  } finally { if (previous === undefined) delete process.env.TZ; else process.env.TZ = previous; }
});

test("DST gaps are rejected and repeated times select the first occurrence", () => {
  const previous = process.env.TZ;
  try {
    process.env.TZ = "America/New_York";
    const draft = { ...timingDraft(), repeat: "once", date: "2027-03-14", time: "02:30" };
    assert.equal(buildTiming(draft), null);
    assert.deepEqual(buildTiming({ ...draft, date: "2027-11-07", time: "01:30" }), { kind: "once", at: "2027-11-07T05:30:00.000Z" });
  } finally { if (previous === undefined) delete process.env.TZ; else process.env.TZ = previous; }
});
