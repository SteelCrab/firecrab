import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import test from "node:test";
import { openDashboard } from "../src/navigation.mjs";

class Page extends EventEmitter {
  constructor(attempts) {
    super();
    this.attempts = attempts;
    this.visits = [];
    this.reloads = 0;
  }
  async goto(hash) {
    this.visits.push(hash);
    // A second visit to the same hash does not load another document.
    if (this.location === hash) return;
    this.load();
    this.location = hash;
  }
  async reload() {
    this.reloads += 1;
    this.load();
  }
  load() {
    this.current = this.attempts.shift();
    if (this.current.failed) this.emit("requestfailed", {
      resourceType: () => this.current.resource ?? "script",
      failure: () => ({ errorText: this.current.failed }),
    });
    if (this.current.navigationError) throw this.current.error;
  }
  async waitForFunction() { if (this.current.error) throw this.current.error; }
}

test("a mounted dashboard is opened once", async () => {
  const page = new Page([{}]);
  await openDashboard(page, "/#/console/vm");
  assert.deepEqual(page.visits, ["/#/console/vm"]);
  assert.equal(page.reloads, 0);
  assert.equal(page.listenerCount("requestfailed"), 0);
});

for (const navigationError of [false, true]) {
  test(`network-change ${navigationError ? "navigation failure retries navigation" : "module failure reloads the committed document"} once`, async () => {
    const page = new Page([
      { failed: "net::ERR_NETWORK_CHANGED", error: new Error("load interrupted"), navigationError },
      {},
    ]);
    await openDashboard(page, "/#/console/vm");
    assert.deepEqual(page.visits, navigationError ? ["/#/console/vm", "/#/console/vm"] : ["/#/console/vm"]);
    assert.equal(page.reloads, navigationError ? 0 : 1);
    assert.equal(page.listenerCount("requestfailed"), 0);
  });
}

for (const failure of [
  { failed: "net::ERR_CONNECTION_REFUSED" },
  { failed: "net::ERR_NETWORK_CHANGED", resource: "image" },
  {},
]) {
  test(`other load errors stay visible (${JSON.stringify(failure)})`, async () => {
    const error = new Error("dashboard did not mount");
    const page = new Page([{ ...failure, error }]);
    await assert.rejects(openDashboard(page, "/#/console/vm"), (actual) => actual === error);
    assert.equal(page.visits.length, 1);
    assert.equal(page.reloads, 0);
    assert.equal(page.listenerCount("requestfailed"), 0);
  });
}

test("a repeated network change fails after one reload", async () => {
  const error = new Error("still interrupted");
  const failure = { failed: "net::ERR_NETWORK_CHANGED", error };
  const page = new Page([failure, failure]);
  await assert.rejects(openDashboard(page, "/#/console/vm"), (actual) => actual === error);
  assert.equal(page.visits.length, 1);
  assert.equal(page.reloads, 1);
  assert.equal(page.listenerCount("requestfailed"), 0);
});
