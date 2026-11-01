import { defineConfig } from "@playwright/test";
import base from "./playwright.config";
import { dashboardSpecs } from "./src/dashboardSpecs";

// The dashboard job covers fixture suites; console sessions run separately.
export default defineConfig({
  ...base,
  testIgnore: [
    ...(Array.isArray(base.testIgnore) ? base.testIgnore : [base.testIgnore!]),
    dashboardSpecs,
  ],
});
