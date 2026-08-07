export const num = (v: unknown): number => {
  const n = typeof v === "string" ? Number(v) : (v as number);
  return Number.isFinite(n) ? n : 0;
};

export const money = (v: unknown, dp = 2) =>
  num(v).toLocaleString("en-US", { style: "currency", currency: "USD", minimumFractionDigits: dp, maximumFractionDigits: dp });

export const signed = (v: unknown) => (num(v) > 0 ? "+" : "") + money(v);

/** Seconds → the coarsest unit that still says something useful. */
export const ago = (s: number | null | undefined) => {
  if (s == null || !Number.isFinite(s)) return "—";
  if (s < 90) return `${Math.round(s)}s`;
  if (s < 5400) return `${Math.round(s / 60)}m`;
  if (s < 129600) return `${(s / 3600).toFixed(1)}h`;
  return `${Math.round(s / 86400)}d`;
};

export const stamp = (iso?: string | null) =>
  iso ? String(iso).replace("T", " ").replace(/(\.\d+)?(Z|\+00:00)$/, "") : "—";

export const tone = (v: unknown) => (num(v) > 0 ? "text-go" : num(v) < 0 ? "text-alarm" : "text-dim");
