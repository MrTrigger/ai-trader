import { expect, test, type Page } from "@playwright/test";

/**
 * A bot nothing is running.
 *
 * Every state here has been shipped wrong at least once, and the previous
 * suite asserted the pill without ever asserting the buttons — so a state
 * with STOP NOT APPLIED and both Start and Stop offered passed cleanly.
 * This checks the pill AND all three verbs together, because it is the
 * combination that is either coherent or nonsense.
 *
 * crypto-portfolio has no process and trades 24/7, so it can never be
 * "idle for a closed market": the only honest reason it is doing nothing is
 * that nothing runs it.
 */
const BOT = "crypto-portfolio";
const STOP_GRACE_MS = 30_000;
const APPLY_GRACE_MS = 120_000;

const dialogs: string[] = [];

const controlWord = async (page: Page): Promise<string | undefined> =>
  (await (await page.request.get(`/api/bots/${BOT}/state`)).json()).controls?.state;

const pill = (page: Page) => page.locator("h1").locator("..").locator("span.uppercase").first();
const btn = (page: Page, name: string) => page.getByRole("button", { name, exact: true });

async function setControl(page: Page, verb: "resume" | "halt" | "stop", want: string) {
  // Drive state through the API, then assert the UI reflects it. Clicking
  // is covered by controls.spec.ts; here the states themselves are the
  // subject, and some of them take two minutes to reach.
  expect((await page.request.post(`/api/bots/${BOT}/${verb}`)).ok()).toBeTruthy();
  await expect.poll(() => controlWord(page), { timeout: 15_000 }).toBe(want);
}

/** The three verbs as [enabled?] — read straight off the DOM. */
async function verbs(page: Page) {
  const start = (await btn(page, "Start").count()) ? "Start" : "Resume";
  return {
    start: await btn(page, start).isEnabled(),
    halt: await btn(page, "Halt").isEnabled(),
    stop: await btn(page, "Stop").isEnabled(),
    startLabel: start,
  };
}

test.beforeEach(async ({ page }) => {
  dialogs.length = 0;
  page.on("dialog", (d) => { dialogs.push(d.message()); void d.accept(); });
  await page.goto(`/bot/${BOT}`);
  await expect(page.locator("h1")).toHaveText(BOT);
});

test("the page says outright that nothing is running this bot", async ({ page }) => {
  // The standing answer to "why isn't Stop immediate?". It has to be on the
  // page, not only in a toast that has already been dismissed.
  await expect(page.locator("body")).toContainText(/Nothing has run this bot in/i);
  await expect(page.locator("body")).toContainText(/none of these take effect until something runs it/i);
});

test("Start does not make it 'running'", async ({ page }) => {
  test.setTimeout(240_000);
  await setControl(page, "resume", "running");

  // Inside the grace it is a transition, not an accomplished fact. Asserted
  // STRICTLY: an alternation with NOT RUNNING passed in 1.3s against a
  // stale render, proving nothing about the transition it claimed to test.
  // A fresh Start refreshes set_at, so this must appear within a refetch.
  await expect
    .poll(async () => (await pill(page).textContent())?.trim(),
      { timeout: 25_000, intervals: [1_000], message: "a fresh Start never showed as 'starting'" })
    .toMatch(/^STARTING$/i);

  // Past it, the truth: the instruction was recorded and nothing acted.
  await expect
    .poll(async () => (await pill(page).textContent())?.trim(),
      { timeout: APPLY_GRACE_MS + 45_000, intervals: [3_000], message: "never became 'not running'" })
    .toMatch(/NOT RUNNING/i);
  await expect(pill(page)).toHaveClass(/alarm/);

  // Never green, and never "idle" — crypto has no closed market to be idle for.
  await expect(pill(page)).not.toHaveClass(/text-go/);
  await expect(pill(page)).not.toContainText(/IDLE/i);

  const v = await verbs(page);
  expect(v, "running: only Halt and Stop change anything").toMatchObject({
    start: false, halt: true, stop: true,
  });
});

test("an unapplied Stop offers Start and nothing else", async ({ page }) => {
  test.setTimeout(180_000);
  await setControl(page, "resume", "running");
  await setControl(page, "stop", "stopped");

  await expect
    .poll(async () => (await pill(page).textContent())?.trim(),
      { timeout: STOP_GRACE_MS + 45_000, intervals: [2_000] })
    .toMatch(/NOT APPLIED|NOT STOPPED/i);

  // The state the screenshot caught: both Start and Stop enabled, which
  // asks the operator to guess what a second Stop would do. It does
  // nothing — the bot is already stopped and nothing is listening.
  const v = await verbs(page);
  expect(v.startLabel, "out of stopped the verb is Start, not Resume").toBe("Start");
  expect(v, "stopped + unreachable: only Start changes anything").toMatchObject({
    start: true, halt: false, stop: false,
  });
});

test("an unapplied Halt is reported too", async ({ page }) => {
  test.setTimeout(240_000);
  await setControl(page, "resume", "running");
  await setControl(page, "halt", "halted");

  await expect
    .poll(async () => (await pill(page).textContent())?.trim(),
      { timeout: APPLY_GRACE_MS + 45_000, intervals: [3_000] })
    .toMatch(/NOT HALTED|HALT NOT APPLIED/i);

  const v = await verbs(page);
  expect(v.startLabel, "out of halted the verb is Resume").toBe("Resume");
  expect(v, "halted: Resume back, or Stop to flatten").toMatchObject({
    start: true, halt: false, stop: true,
  });
});

test.afterAll(async ({ request }) => {
  // Leave it as the operator had it.
  await request.post(`/api/bots/${BOT}/stop`);
});
