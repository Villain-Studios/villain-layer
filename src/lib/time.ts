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
