/**
 * The phone's page (PHONE-*) against a fake Mac, for looking and clicking.
 * Open /phone-mock.html on the dev Vite; nothing here is in the build.
 *
 *   ?paired=0     start unpaired: the code is 482913
 *   &typing=0     typing switched off on the Mac
 *   &theme=light  the Mac's theme: dark (default), light, tokyo-night, system (SET-5)
 *   #pane=p-ask   open straight on a pane
 *
 * From the console:
 *
 *   __phone.calls                  every request, with its body
 *   __phone.print("p-ask", "text") output for a pane's open stream
 *   __phone.world.groups[0].panes[0].activity = "idle"; __phone.change()
 *   __phone.forget()               the Mac forgets this phone
 */
import type { Group, Overview } from "../phone/types";

const params = new URLSearchParams(location.search);
const ago = (s: number) => new Date(Date.now() - s * 1000).toISOString();

const groups: Group[] = [
  {
    id: "t-login", name: "ACME-123 Fix the login race", key: "ACME-123",
    panes: [
      { id: "p-ask", name: "Retry the token refresh · api", kind: "agent", agent: "claude", activity: "asking", since: ago(90), notice: null, running: true, waiting: "is asking for your permission" },
      { id: "p-shell", name: "Shell · web", kind: "shell", agent: null, activity: "idle", since: ago(4000), notice: null, running: true, waiting: null },
    ],
  },
  {
    id: "t-audit", name: "Audit log for exports", key: "ACME-131",
    panes: [
      { id: "p-work", name: "Claude Code", kind: "agent", agent: "claude", activity: "working", since: ago(300), notice: null, running: true, waiting: null },
    ],
  },
  { id: "t-docs", name: "Setup guide typos", key: "ACME-140", panes: [] },
];

const theme = (params.get("theme") ?? "dark") as Overview["theme"];
const world: Overview = { device: "iPhone", typing: params.get("typing") !== "0", theme, groups };
const calls: { path: string; body: unknown }[] = [];
const streams = new Map<string, Set<ReadableStreamDefaultController<Uint8Array>>>();
const enc = new TextEncoder();
let paired = params.get("paired") !== "0";

if (paired) localStorage.setItem("villain.phone.token", "mock-token");
else localStorage.removeItem("villain.phone.token");

function b64(text: string): string {
  return btoa(String.fromCharCode(...enc.encode(text)));
}

let end = 0;
function push(key: string, line: unknown) {
  for (const c of streams.get(key) ?? []) c.enqueue(enc.encode(`${JSON.stringify(line)}\n`));
}

function print(pane: string, text: string) {
  end += enc.encode(text).length;
  push(pane, { data: b64(text), end, reset: false });
}

/** Roughly what Claude Code shows at a permission question. */
const SCREEN =
  "\x1b[1m\x1b[35m✻\x1b[0m Retrying the refresh with backoff\r\n\r\n" +
  "  \x1b[32m⏺\x1b[0m Read(src/auth/refresh.ts)\r\n" +
  "  \x1b[32m⏺\x1b[0m Update(src/auth/refresh.ts)\r\n    \x1b[2m+ 14 lines, - 3 lines\x1b[0m\r\n\r\n" +
  "╭──────────────────────────────────────────────────────────────────────────────────────────────────╮\r\n" +
  "│ \x1b[1mBash command\x1b[0m                                                                                     │\r\n" +
  "│   npm test -- src/auth                                                                           │\r\n" +
  "│ Do you want to proceed?                                                                          │\r\n" +
  "│ \x1b[36m❯ 1. Yes\x1b[0m                                                                                         │\r\n" +
  "│   2. Yes, and don't ask again for npm test commands                                              │\r\n" +
  "│   3. No, and tell Claude what to do differently (esc)                                            │\r\n" +
  "╰──────────────────────────────────────────────────────────────────────────────────────────────────╯\r\n";

function stream(key: string, first: unknown[]): Response {
  let mine: ReadableStreamDefaultController<Uint8Array>;
  const body = new ReadableStream<Uint8Array>({
    start(c) {
      mine = c;
      if (!streams.has(key)) streams.set(key, new Set());
      streams.get(key)!.add(c);
      for (const line of first) c.enqueue(enc.encode(`${JSON.stringify(line)}\n`));
    },
    cancel() {
      streams.get(key)?.delete(mine);
    },
  });
  return new Response(body, { headers: { "Content-Type": "application/x-ndjson" } });
}

const json = (v: unknown, status = 200) =>
  new Response(JSON.stringify(v), { status, headers: { "Content-Type": "application/json" } });

window.fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
  const path = typeof input === "string" ? input : input instanceof URL ? input.pathname + input.search : input.url;
  const body = init?.body ? JSON.parse(String(init.body)) : null;
  calls.push({ path, body });
  const url = new URL(path, location.origin);

  if (url.pathname === "/api/pair") {
    if (body?.code !== "482913") return json({ error: "That is not the code showing on the Mac." }, 403);
    paired = true;
    return json({ token: "mock-token", device: "ph-1", name: body.name });
  }
  if (!paired) return json({ error: "This phone is not paired with the app, or was forgotten." }, 401);
  if (url.pathname === "/api/overview") return json(structuredClone(world));
  if (url.pathname === "/api/changes") return stream("changes", []);

  const m = /^\/api\/panes\/([^/]+)\/(output|input)$/.exec(url.pathname);
  const pane = m && world.groups.flatMap((g) => g.panes).find((p) => p.id === decodeURIComponent(m[1]));
  if (!m || !pane) return json({ error: "That terminal is closed." }, 404);
  if (m[2] === "output") {
    const text = pane.id === "p-ask" ? SCREEN : "you@mac web % ";
    end = enc.encode(text).length;
    return stream(pane.id, [{ size: [30, 100] }, { data: b64(text), end, reset: false }]);
  }
  if (!world.typing) return json({ error: "Typing from the phone is off. Turn it on in the app's Settings, under Phone." }, 403);
  if (pane.kind !== "agent") return json({ error: "Only agents take typing from the phone, not shells." }, 403);
  print(pane.id, body.text ? `\r\n\x1b[2m> ${body.text}\x1b[0m\r\n` : `\x1b[2m[${body.key}]\x1b[0m`);
  return new Response(null, { status: 204 });
};

(window as unknown as { __phone: unknown }).__phone = {
  calls,
  world,
  print,
  change: () => push("changes", { changed: true }),
  forget: () => {
    paired = false;
    push("changes", { changed: true });
  },
};

await import("../phone/main");
