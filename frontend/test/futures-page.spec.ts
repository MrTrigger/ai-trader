import { expect, test, type Page } from "@playwright/test";

/**
 * The futures bot's OWN page, driven end to end.
 *
 * Both control suites drove crypto-portfolio, so "the pill reflects a
 * Stop" was only ever proven for the runner contract — and the botstate
 * branch of the detail endpoint turned out not to send `controls` at all.
 * The operator pressed Stop on a live bot, the bot obeyed in 0.2s, and
 * the page kept showing RUNNING·IDLE with Resume enabled because it had
 * no control document to read. Every assertion here runs against the
 * contract that actually trades.
 *
 * This bot is live (paper account): the spec refuses to touch it unless
 * the book is flat, and puts the control word back exactly as it found it.
 */
const BOT = "futures-noise";

const pill = (page: Page) => page.locator("h1").locator("..").locator("span.uppercase").first();
const btn = (page: Page, name: string) => page.getByRole("button", { name, exact: true });

async function fleetStatus(page: Page) {
  const bots = (await (await page.request.get("/api/bots")).json()).bots;
  return bots.find((b: { bot_id: string }) => b.bot_id === BOT)?.status;
}

test("the futures page reflects its controls, fast", async ({ page }) => {
  test.setTimeout(90_000);
  page.on("dialog", (d) => void d.accept());

  const before = await fleetStatus(page);
  expect(before, `${BOT} is not reporting — is it running?`).toBeTruthy();
  expect(before.headline?.fills ?? 0, "refusing to drive a bot with fills today").toBe(0);
  const initialControl: string = before.control_state;

  await page.goto(`/bot/${BOT}`);
  await expect(page.locator("h1")).toHaveText(BOT);

  // The regression itself: whatever the control word is, the page must not
  // be drawing it from thin air. A missing document renders the "trading"
  // placeholder caption; the real word renders the word.
  await expect(page.locator("body")).toContainText(/CONTROLS/i);
  await expect(page.locator("body")).not.toContainText(/CONTROLS\s*trading/);

  // Bring it to running (possibly a no-op), then exercise a full stop and
  // start from the page, asserting the pill each time. The budget is
  // seconds, not minutes: the control is pushed and the page bursts its
  // refetch after a press.
  if (initialControl !== "running") {
    await btn(page, "Start").click();
    await expect
      .poll(async () => (await pill(page).textContent())?.trim(), { timeout: 10_000, intervals: [500] })
      .toMatch(/^RUNNING/i);
  }

  await btn(page, "Stop").click();
  await expect
    .poll(async () => (await pill(page).textContent())?.trim(),
      { timeout: 10_000, intervals: [500], message: "the pill never showed the stop" })
    .toMatch(/^STOP/i);
  // Acknowledged (the bot publishes on the control change), so this is the
  // settled state, not the in-flight one — the bot's own reason rides
  // along ("stopped · operator-stop"), which is the acknowledgement.
  await expect
    .poll(async () => (await pill(page).textContent())?.trim(), { timeout: 10_000, intervals: [500] })
    .toMatch(/^stopped/i);
  await expect(btn(page, "Start")).toBeEnabled();
  await expect(btn(page, "Halt")).toBeDisabled();
  await expect(btn(page, "Stop")).toBeDisabled(); // flat + stopped: nothing left to close

  await btn(page, "Start").click();
  await expect
    .poll(async () => (await pill(page).textContent())?.trim(), { timeout: 10_000, intervals: [500] })
    .toMatch(/^RUNNING/i);

  // Leave the bot exactly as the operator had it.
  if (initialControl !== "running") {
    await page.request.post(`/api/bots/${BOT}/${initialControl === "stopped" ? "stop" : "halt"}`);
    await expect
      .poll(async () => (await fleetStatus(page)).control_state, { timeout: 10_000 })
      .toBe(initialControl);
  }
});
