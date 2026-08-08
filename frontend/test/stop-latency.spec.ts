import { expect, test, type APIRequestContext } from "@playwright/test";

/**
 * How long a RUNNING bot takes to obey Stop — measured, because the claim
 * was wrong the first time it was measured.
 *
 * Reading controls every 5s is not the same as the operator being able to
 * see it: the bot published status every 60s, so a 5s reaction was
 * invisible for up to a minute and read as "nothing is listening". This
 * pins the end-to-end number, press to visible.
 *
 * It stops and restarts futures-noise, so it refuses to run unless the book
 * is flat, and restores running at the end.
 */
const BOT = "futures-noise";
/** Loop tick is 5s and a control change publishes at once; 15s is slack. */
const OBEY_WITHIN_MS = 15_000;

async function status(api: APIRequestContext) {
  const bots = (await (await api.get("/api/bots")).json()).bots;
  return bots.find((b: { bot_id: string }) => b.bot_id === BOT)?.status;
}

test("a running bot obeys Stop in seconds, and Start brings it back", async ({ request }) => {
  const before = await status(request);
  expect(before, `${BOT} is not reporting — is it running?`).toBeTruthy();
  expect(before.control_state, "expected to start from running").toBe("running");
  // Never exercise this against an open book.
  expect(before.headline?.fills ?? 0, "refusing to Stop a bot with fills today").toBe(0);
  expect(before.halted ?? null).toBeNull();

  const t0 = Date.now();
  expect((await request.post(`/api/bots/${BOT}/stop`)).ok()).toBeTruthy();

  await expect
    .poll(async () => (await status(request)).halted, {
      timeout: OBEY_WITHIN_MS,
      intervals: [250],
      message: "the bot did not apply Stop in time",
    })
    .toBe("operator-stop");
  const obeyed = Date.now() - t0;
  console.log(`  stop applied and published in ${(obeyed / 1000).toFixed(2)}s`);
  expect(obeyed).toBeLessThan(OBEY_WITHIN_MS);

  // Start clears an operator stop — the transition the dashboard's Start
  // depends on, and the one that would make the button a no-op if broken.
  expect((await request.post(`/api/bots/${BOT}/resume`)).ok()).toBeTruthy();
  await expect
    .poll(async () => (await status(request)).halted ?? null, { timeout: OBEY_WITHIN_MS, intervals: [250] })
    .toBeNull();

  const after = await status(request);
  expect(after.control_state).toBe("running");
});
