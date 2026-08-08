import * as ui from "./uidrive";

const API = "http://localhost:7434";
const BOT = "crypto-portfolio";
const control = async () =>
  (await (await fetch(`${API}/api/bots/${BOT}/state`)).json()).controls?.state;

let failures = 0;
function check(name: string, pass: boolean, got?: unknown) {
  console.log(`${pass ? "PASS" : "FAIL"}  ${name}${pass ? "" : `  → got: ${JSON.stringify(got)}`}`);
  if (!pass) failures++;
}

await ui.goto(`${API}/bot/${BOT}`);
await ui.stubConfirm();

// --- start out of stopped -------------------------------------------------
console.log(`\n[control before] ${await control()}`);
check("primary button reads Start when stopped", (await ui.buttonState("Start")) !== "missing",
  await ui.buttonState("Start"));
await ui.clickButton("Start");
await ui.waitFor(async () => (await control()) === "running");
check("Start writes control=running", (await control()) === "running");
check("no toast promises a cycle", !(await ui.bodyHas("next cycle")), await ui.bodyHas("next cycle"));

// --- stop ----------------------------------------------------------------
await ui.stubConfirm();
await ui.clickButton("Stop");
await ui.waitFor(async () => (await control()) === "stopped");
check("Stop writes control=stopped", (await control()) === "stopped");

const cm: string = await ui.confirmMessage();
check("Stop confirm says it closes at market now", /at market, now/.test(cm ?? ""), cm);
check("Stop confirm warns nothing is running this bot", /nothing has run this bot/i.test(cm ?? ""), cm);

await Bun.sleep(1500);
check("Stop toast does NOT promise the next cycle", !(await ui.bodyHas("next cycle")));
check("Halt copy describes a wind-down", await ui.bodyHas("wind down"));
check("Halt copy no longer says positions are left alone",
  !(await ui.bodyHas("leaves open positions alone")));

const p = await ui.pill();
console.log(`[pill] ${p}`);
check("pill reflects the stop", /stop/i.test(p ?? ""), p);

// Start must stay reachable out of a stop nothing will apply.
await ui.waitFor(async () => (await ui.buttonState("Start")) !== "missing");
console.log(`[Start button] ${await ui.buttonState("Start")}`);

await ui.shot("/tmp/ui_after_stop.png");
console.log(`\n[control after] ${await control()}`);
console.log(failures === 0 ? "\nALL PASS" : `\n${failures} FAILED`);
await ui.done();
process.exit(failures === 0 ? 0 : 1);
