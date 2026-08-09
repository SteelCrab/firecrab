import { describe, expect, it } from "vitest";

import { isPublishedRegistryObject, registryObjectKey } from "./registry";

describe("registryObjectKey", () => {
  it("keeps a distribution tree key", () => {
    expect(registryObjectKey("/ubuntu/26.04/ubuntu-26.04.tar.zst")).toBe(
      "ubuntu/26.04/ubuntu-26.04.tar.zst",
    );
  });

  it("rejects hidden and traversal-style keys", () => {
    expect(registryObjectKey("/.healthchecks/write-test")).toBeNull();
    expect(registryObjectKey("/ubuntu/%2E%2E/catalog.json")).toBeNull();
    expect(registryObjectKey("/ubuntu//SHA256SUMS")).toBeNull();
  });
});

describe("isPublishedRegistryObject", () => {
  it("allows only catalog and published package files", () => {
    expect(isPublishedRegistryObject("catalog.json")).toBe(true);
    expect(isPublishedRegistryObject("rocky/9/rocky-9.tar.zst")).toBe(true);
    expect(isPublishedRegistryObject("rocky/9/SHA256SUMS")).toBe(true);
    expect(isPublishedRegistryObject(".healthchecks/write-test")).toBe(false);
    expect(isPublishedRegistryObject("private/credentials.json")).toBe(false);
  });
});
