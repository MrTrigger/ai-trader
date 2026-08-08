/**
 * Two claims I have not earned yet:
 *   1. an unapplied Stop turns red rather than sitting in "stopping"
 *   2. Stop is now acted on in seconds, not at the next bar
 *
 * (2) needs a bot that is actually running. futures-noise is flat with the
 * market closed, so a Stop there is a no-op flatten; it is restored to
 * running at the end and asserted.
 */
const API = "http://localhost:7434";
let failures = 0;
const check = (n: string, ok: boolean, got?: unknown) => {
  console.log(`${ok ? "PASS" : "FAIL"}  ${n}${ok ? "" : `  → ${JSON.stringify(got)}`}`);
  if (!ok) failures++;
};
const sleep = (ms: number) => Bun.sleep(ms);
const fleet = async () => (await (await fetch(`${API}/api/bots`)).json()).bots;
const one = async (id: string) => (await fleet()).find((b: any) => b.bot_id === id);

// ---- 1. the red state ----------------------------------------------------
import * as ui from "./uidrive";
await ui.goto(`${API}/bot/crypto-portfolio`);
console.log(`[crypto pill now] ${await ui.pill()}`);
console.log("waiting out the 30s stop grace…");
await sleep(34_000);
await ui.goto(`${API}/bot/crypto-portfolio`); // refetch
const p = await ui.pill();
check("an unapplied Stop goes red, not 'stopping'", /not applied|not stopped/i.test(p ?? ""), p);
await ui.shot("/tmp/ui_stop_not_applied.png");

// ---- 2. how fast a RUNNING bot obeys Stop --------------------------------
const before = await one("futures-noise");
check("futures-noise starts from running", before.status.control_state === "running",
  before.status.control_state);
check("futures-noise is flat before we touch it", (before.status.trades_total ?? 0) === 0 &&
  before.status.headline?.fills === 0, before.status.headline);

const t0 = Date.now();
await fetch(`${API}/api/bots/futures-noise/stop`, { method: "POST" });
let acked = -1;
for (let i = 0; i < 60; i++) {
  const b = await one("futures-noise");
  if (b?.status?.halted) { acked = (Date.now() - t0) / 1000; break; }
  await sleep(1000);
}
check("a running bot applies Stop within 15s", acked >= 0 && acked < 15, `${acked}s`);
console.log(`[stop acknowledged after] ${acked}s  (loop tick is 5s)`);
const stopped = await one("futures-noise");
check("the bot reports why it stopped", stopped.status.halted === "operator-stop",
  stopped.status.halted);

// ---- restore -------------------------------------------------------------
await fetch(`${API}/api/bots/futures-noise/resume`, { method: "POST" });
let back = -1;
for (let i = 0; i < 60; i++) {
  const b = await one("futures-noise");
  if (b?.status?.control_state === "running" && !b?.status?.halted) { back = (Date.now() - t0) / 1000; break; }
  await sleep(1000);
}
const after = await one("futures-noise");
check("Start clears the stop on a running bot", after.status.halted == null, after.status.halted);
check("restored to running", after.status.control_state === "running", after.status.control_state);
console.log(`[resume acknowledged] ${back >= 0 ? "yes" : "NO"}`);

console.log(failures === 0 ? "\nALL PASS" : `\n${failures} FAILED`);
await ui.done();
process.exit(failures === 0 ? 0 : 1);
