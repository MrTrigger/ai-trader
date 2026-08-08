/**
 * Minimal CDP driver: launch the installed chromium, click real buttons,
 * read the real DOM. Enough to stop me claiming a UI works because it
 * compiled.
 */
const CHROME = "/home/magnus/.cache/ms-playwright/chromium-1228/chrome-linux64/chrome";
const PORT = 9333;
const URL = process.argv[2] ?? "http://localhost:7434/";

const proc = Bun.spawn(
  [CHROME, "--headless=new", "--disable-gpu", "--no-sandbox", `--remote-debugging-port=${PORT}`,
   "--window-size=1440,900", "about:blank"],
  { stdout: "ignore", stderr: "ignore" },
);

async function waitFor<T>(fn: () => Promise<T | null>, ms = 20000, every = 200): Promise<T> {
  const until = Date.now() + ms;
  for (;;) {
    try {
      const v = await fn();
      if (v != null && v !== false) return v as T;
    } catch {}
    if (Date.now() > until) throw new Error("timed out waiting");
    await Bun.sleep(every);
  }
}

const target: any = await waitFor(async () => {
  const r = await fetch(`http://127.0.0.1:${PORT}/json/new?${encodeURIComponent(URL)}`, { method: "PUT" });
  return r.ok ? await r.json() : null;
});

const ws = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((res) => (ws.onopen = res));
let id = 0;
const pend = new Map<number, (v: any) => void>();
ws.onmessage = (e) => {
  const m = JSON.parse(String(e.data));
  if (m.id && pend.has(m.id)) { pend.get(m.id)!(m); pend.delete(m.id); }
};
function send(method: string, params: any = {}): Promise<any> {
  const n = ++id;
  return new Promise((res) => { pend.set(n, res); ws.send(JSON.stringify({ id: n, method, params })); });
}
async function evaluate(expression: string) {
  const r = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
  if (r.result?.exceptionDetails) throw new Error(JSON.stringify(r.result.exceptionDetails));
  return r.result?.result?.value;
}

await send("Page.enable");
await send("Runtime.enable");

export async function goto(url: string) {
  await send("Page.navigate", { url });
  await Bun.sleep(1200);
}
export async function text(sel: string) {
  return await evaluate(`document.querySelector(${JSON.stringify(sel)})?.textContent?.trim() ?? null`);
}
/** Click the button whose visible text matches, exactly. */
export async function clickButton(label: string) {
  const ok = await evaluate(`(() => {
    const b = [...document.querySelectorAll("button")].find(x => x.textContent.trim() === ${JSON.stringify(label)});
    if (!b) return "missing";
    if (b.disabled) return "disabled";
    b.click(); return "clicked";
  })()`);
  if (ok !== "clicked") throw new Error(`button ${label}: ${ok}`);
}
export async function buttonState(label: string) {
  return await evaluate(`(() => {
    const b = [...document.querySelectorAll("button")].find(x => x.textContent.trim() === ${JSON.stringify(label)});
    return b ? (b.disabled ? "disabled" : "enabled") : "missing";
  })()`);
}
export async function stubConfirm() {
  await evaluate(`(() => { window.__cm = null; window.confirm = (m) => { window.__cm = m; return true; }; return 1; })()`);
}
export async function confirmMessage() {
  return await evaluate(`window.__cm`);
}
/** The status pill is the first pill in the header row. */
export async function pill() {
  return await evaluate(`(() => {
    const h = document.querySelector("h1"); if (!h) return null;
    const row = h.parentElement;
    const s = [...row.querySelectorAll("span")].find(x => /uppercase/.test(x.className));
    return s?.textContent?.trim() ?? null;
  })()`);
}
export async function bodyHas(s: string) {
  return await evaluate(`document.body.innerText.includes(${JSON.stringify(s)})`);
}
export async function shot(path: string) {
  const r = await send("Page.captureScreenshot", { format: "png" });
  await Bun.write(path, Buffer.from(r.result.data, "base64"));
}
export async function done() { ws.close(); proc.kill(); }
export { waitFor };
