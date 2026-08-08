import { defineConfig, devices } from "@playwright/test";

/**
 * These run against a LIVE api and real bots — they are operational checks,
 * not a hermetic suite. Hence one worker and no retries: a retry would
 * press Stop on a live bot a second time and hide a flaky control path,
 * which is the exact thing being tested.
 */
export default defineConfig({
  testDir: "./test",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  timeout: 120_000,
  reporter: [["list"]],
  use: {
    baseURL: process.env.API_BASE ?? "http://localhost:7434",
    ...devices["Desktop Chrome"],
    viewport: { width: 1440, height: 900 },
    screenshot: "only-on-failure",
  },
});
