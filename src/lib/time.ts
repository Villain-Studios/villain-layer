/** How long ago, in a word: "just now", "4m ago", "3d ago". */
export function ago(when: string | number, now: number = Date.now()): string {
  const t = typeof when === "number" ? when : Date.parse(when);
  if (Number.isNaN(t)) return "";
  const s = Math.max(0, Math.round((now - t) / 1000));
  if (s < 5) return "just now";
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  if (s < 86400) return `${Math.round(s / 3600)}h ago`;
  return `${Math.round(s / 86400)}d ago`;
}

/**
 * How old, as short as it goes: "5m", "3h", "2d", "1w", "4mo", "2y" (TKT-11).
 * For a list where every row has one, so the word "ago" would only repeat.
 */
export function age(when: string | number, now: number = Date.now()): string {
  const t = typeof when === "number" ? when : Date.parse(when);
  if (Number.isNaN(t)) return "";
  const m = Math.max(0, Math.floor((now - t) / 60_000));
  if (m < 1) return "now";
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h`;
  const d = Math.floor(h / 24);
  if (d < 7) return `${d}d`;
  if (d < 30) return `${Math.floor(d / 7)}w`;
  if (d < 365) return `${Math.floor(d / 30)}mo`;
  return `${Math.floor(d / 365)}y`;
}
