import { expect, test, type Page } from "@playwright/test";

/**
 * The control verbs, pressed for real.
 *
 * These exist because "it typechecks" and "it says the right thing" turned
 * out to be different claims twice in a row: the Stop toast kept promising
 * "the book closes at the bot's next cycle" long after that stopped being
 * true, and nothing caught it but a human looking at the screen.
 *
 * crypto-portfolio is the subject: it has no process running it, so its
 * controls can be exercised without any market consequence — and it is the
 * case that matters most, because a control nothing is listening for must
 * not look like one that was obeyed.
 */
const BOT = "crypto-portfolio";

const controlWord = async (page: Page): Promise<string | undefined> => {
  const r = await page.request.get(`/api/bots/${BOT}/state`);
  return (await r.json()).controls?.state;
};

/** The status pill: the uppercase chip beside the bot name. */
const pill = (page: Page) =>
  page.locator("h1").locator("..").locator("span.uppercase").first();

/**
 * Press Start, waiting for it to become available first.
 *
 * Start is deliberately disabled while a Stop is in flight, so a test that
 * follows another test's Stop lands inside the 30s grace and finds it
 * disabled. That is the product behaving correctly; the test has to wait it
 * out rather than assert against a transient.
 */
async function clickStart(page: Page) {
  const start = page.getByRole("button", { name: "Start", exact: true });
  await expect(start).toBeEnabled({ timeout: 45_000 });
  await start.click();
}

async function waitForControl(page: Page, want: string) {
  await expect
    .poll(() => controlWord(page), { timeout: 20_000, message: `control never became ${want}` })
    .toBe(want);
}

/**
 * Confirmation text, in order. ONE handler accepts and records — two
 * handlers both calling accept() throws "already handled", which is what a
 * per-test `page.once` on top of a blanket `page.on` did.
 */
const dialogs: string[] = [];

test.beforeEach(async ({ page }) => {
  dialogs.length = 0;
  page.on("dialog", (d) => {
    dialogs.push(d.message());
    void d.accept();
  });
  await page.goto(`/bot/${BOT}`);
  await expect(page.locator("h1")).toHaveText(BOT);
});

test("Stop is a door you can come back through", async ({ page }) => {
  // Regression: `Controls` took a boolean `halted`, which cannot hold three
  // states, so a stopped bot read as neither halted nor running and the way
  // back was disabled. Stop was one-way from the dashboard.
  await waitForControl(page, "stopped");
  const start = page.getByRole("button", { name: "Start", exact: true });
  await expect(start).toBeVisible();

  await clickStart(page);
  await waitForControl(page, "running");
  await expect(page.getByRole("button", { name: "Halt", exact: true })).toBeEnabled();

  await page.getByRole("button", { name: "Stop", exact: true }).click();
  await waitForControl(page, "stopped");
});

test("nothing promises a cycle that may never come", async ({ page }) => {
  // The exact string that shipped wrong twice.
  await waitForControl(page, "stopped");
  await clickStart(page);
  await waitForControl(page, "running");
  await expect(page.locator("body")).not.toContainText("next cycle");

  await page.getByRole("button", { name: "Stop", exact: true }).click();
  await waitForControl(page, "stopped");
  await expect(page.locator("body")).not.toContainText("next cycle");
});

// The 30s stop grace has to elapse inside this one.
test("a control nothing is listening for says so, in red", async ({ page }) => {
  test.setTimeout(180_000);
  // This bot has no process. The dashboard must not imply the book closed.
  await waitForControl(page, "stopped");
  await clickStart(page);
  await waitForControl(page, "running");

  await page.getByRole("button", { name: "Stop", exact: true }).click();
  await expect.poll(() => dialogs.length, { timeout: 10_000 }).toBeGreaterThan(0);
  const message = dialogs.at(-1) ?? "";
  expect(message).toContain("at market, now");
  // The warning that makes this bot's Stop honest: nothing will act on it.
  expect(message).toMatch(/nothing has run this bot/i);

  await waitForControl(page, "stopped");
  await expect(page.locator("body")).toContainText(/nothing will act on it yet/i);

  // Past the grace the pill stops being a transition and becomes a fault.
  // No reload needed: the page refetches on its own and unackSeconds is
  // computed at render time, so this flips without any help.
  await expect
    .poll(async () => (await pill(page).textContent())?.trim(), {
      timeout: 75_000,
      intervals: [2_000],
      message: "an unapplied stop never became a fault",
    })
    .toMatch(/NOT APPLIED|NOT STOPPED/i);
  await expect(pill(page)).toHaveClass(/alarm/);

  // ...and Start must remain reachable out of a stop nothing will apply.
  await expect(page.getByRole("button", { name: "Start", exact: true })).toBeEnabled();
});

test("Halt promises a wind-down, not a freeze", async ({ page }) => {
  // The runtime used to skip the whole bar while halted, so an open
  // position sailed through its own stop-loss. The copy claimed positions
  // were "left alone" and that was true in the worst sense.
  await expect(page.locator("body")).toContainText("wind down");
  await expect(page.locator("body")).toContainText(/still close when the strategy exits them/i);
  await expect(page.locator("body")).not.toContainText("leaves open positions alone");
});
