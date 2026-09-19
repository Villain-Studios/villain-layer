/**
 * Small guarded wrapper over localStorage, used for the bits of layout that
 * should survive a restart: which task was open, which tab, which pane.
 *
 * Every access is wrapped because the webview can refuse storage outright, and
 * a half-written value should cost a preference rather than the whole app.
 */
const PREFIX = "villain.";

export function read<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(PREFIX + key);
    return raw === null ? fallback : (JSON.parse(raw) as T);
  } catch {
    return fallback;
  }
}

/** Like `read`, but a value that is no longer one of `allowed` is discarded. */
export function readOneOf<T extends string>(key: string, allowed: readonly T[], fallback: T): T {
  const v = read<T>(key, fallback);
  return allowed.includes(v) ? v : fallback;
}

export function write(key: string, value: unknown): void {
  try {
    localStorage.setItem(PREFIX + key, JSON.stringify(value));
  } catch {
    // Storage disabled or full: the preference simply does not stick.
  }
}
