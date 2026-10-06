/**
 * The phone's way to the Mac: plain `fetch` to the app's phone server, with
 * the token this phone was given when it paired.
 *
 * Streams are lines of JSON read through `fetch`, not `EventSource`, which
 * cannot send the token as a header; a token in the URL would sit in the
 * history and any log on the way.
 */
import type { Overview } from "./types";

const TOKEN = "villain.phone.token";

/** The token, or null before pairing. Storage can be refused (private mode). */
export function token(): string | null {
  try {
    return localStorage.getItem(TOKEN);
  } catch {
    return null;
  }
}

function setToken(t: string | null) {
  try {
    if (t) localStorage.setItem(TOKEN, t);
    else localStorage.removeItem(TOKEN);
  } catch {
    // Paired for as long as the page is open, then.
  }
  window.dispatchEvent(new Event("villain-token"));
}

/** The Mac no longer knows this phone: forgotten in Settings, or never paired. */
export class Unpaired extends Error {}

async function failure(res: Response): Promise<Error> {
  if (res.status === 401) {
    setToken(null);
    return new Unpaired("This phone is no longer paired with the app.");
  }
  const body = (await res.json().catch(() => null)) as { error?: string } | null;
  return new Error(body?.error ?? `The Mac answered ${res.status}.`);
}

async function call<T>(path: string, init: RequestInit = {}): Promise<T> {
  const res = await fetch(path, {
    ...init,
    headers: { "Content-Type": "application/json", Authorization: `Bearer ${token() ?? ""}` },
  });
  if (!res.ok) throw await failure(res);
  return (res.status === 204 ? undefined : await res.json()) as T;
}

export const phoneApi = {
  /** Trade the code showing on the Mac for this phone's own token. */
  async pair(code: string, name: string): Promise<void> {
    const res = await fetch("/api/pair", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ code, name }),
    });
    if (!res.ok) throw await failure(res);
    const body = (await res.json()) as { token: string };
    setToken(body.token);
  },
  overview: () => call<Overview>("/api/overview"),
  /** A line and Enter, into an agent. */
  send: (pane: string, text: string) =>
    call<void>(`/api/panes/${encodeURIComponent(pane)}/input`, { method: "POST", body: JSON.stringify({ text }) }),
  /** One key of the key bar. */
  key: (pane: string, key: string) =>
    call<void>(`/api/panes/${encodeURIComponent(pane)}/input`, { method: "POST", body: JSON.stringify({ key }) }),
};

/**
 * The server writes at least a blank line every 20 seconds. Nothing for this
 * long, and the connection is taken for dead: a phone that slept, or changed
 * networks, keeps a socket that will never say anything again.
 */
const SILENCE = 45_000;

/**
 * Follow a stream of JSON lines, opening it again whenever it drops, until
 * the returned function is called. `path` is asked again on every opening,
 * so a stream can resume from where it got to. `ended` returns true when the
 * stream finished for good (the pane exited) rather than dropped.
 */
export function follow(
  path: () => string,
  onLine: (line: unknown) => boolean | void,
  onLive: (live: boolean) => void,
): () => void {
  let stopped = false;
  let abort: AbortController | null = null;
  let retry: number | undefined;
  let wait = 1000;
  let active = false;
  /** The stream said it was over: nothing to open again. */
  let ended = false;

  async function open() {
    if (stopped) return;
    active = true;
    abort = new AbortController();
    const mine = abort;
    let quiet: number | undefined;
    const hush = () => {
      window.clearTimeout(quiet);
      quiet = window.setTimeout(() => mine.abort(), SILENCE);
    };
    try {
      hush();
      const res = await fetch(path(), {
        headers: { Authorization: `Bearer ${token() ?? ""}` },
        signal: mine.signal,
      });
      if (res.status === 404) {
        ended = true;
        onLine({ gone: true });
        return;
      }
      if (!res.ok || !res.body) throw await failure(res);
      onLive(true);
      wait = 1000;
      const reader = res.body.getReader();
      const text = new TextDecoder();
      let held = "";
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        hush();
        held += text.decode(value, { stream: true });
        let nl: number;
        while ((nl = held.indexOf("\n")) !== -1) {
          const line = held.slice(0, nl).trim();
          held = held.slice(nl + 1);
          if (line && onLine(JSON.parse(line)) === true) ended = true;
        }
      }
    } catch (e) {
      if (e instanceof Unpaired) stopped = true;
    } finally {
      window.clearTimeout(quiet);
      active = false;
    }
    if (stopped || ended) return;
    onLive(false);
    retry = window.setTimeout(() => void open(), wait);
    wait = Math.min(wait * 2, 15_000);
  }

  // Back from the lock screen: the old connection is likely dead already,
  // and waiting out the silence would show a stale screen for 45 seconds.
  const wake = () => {
    if (document.visibilityState !== "visible" || stopped || ended) return;
    window.clearTimeout(retry);
    wait = 1000;
    // An open one is dropped and opens again; one waiting to retry, now.
    if (active) abort?.abort();
    else void open();
  };
  document.addEventListener("visibilitychange", wake);
  void open();
  return () => {
    stopped = true;
    window.clearTimeout(retry);
    abort?.abort();
    document.removeEventListener("visibilitychange", wake);
  };
}
