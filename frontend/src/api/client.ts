import { BotDetail, Overview } from "./types";

async function get<T>(url: string, parse: (v: unknown) => T): Promise<T> {
  const r = await fetch(url, { headers: { accept: "application/json" } });
  if (!r.ok) throw new Error(`${r.status} on ${url}`);
  return parse(await r.json());
}

export const api = {
  overview: () => get("/api/fleet/overview", (v) => Overview.parse(v)),
  detail: (id: string) =>
    get(`/api/bots/${encodeURIComponent(id)}/state`, (v) => BotDetail.parse(v)),

  /** The three control verbs. Halt leaves the book alone; stop closes it. */
  async control(id: string, verb: "resume" | "halt" | "stop") {
    const r = await fetch(`/api/bots/${encodeURIComponent(id)}/${verb}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "{}",
    });
    const body = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error((body as { error?: string }).error ?? `HTTP ${r.status}`);
    return body;
  },

  async setAccount(id: string, account_id: string) {
    const r = await fetch(`/api/bots/${encodeURIComponent(id)}/venue`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ account_id }),
    });
    const body = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error((body as { error?: string }).error ?? `HTTP ${r.status}`);
    return body;
  },
};
