/**
 * A pretend ACP agent (§20) for the mock harness: a conversation per pane,
 * answering prompts with a tool call and a streamed reply, and asking
 * permission whenever a prompt says "write". Enough to look at and click
 * through every kind of entry; nothing like a real agent's timing.
 */
import { emit } from "@tauri-apps/api/event";
import type { AcpEntry, AcpSetting, AcpView } from "../lib/types";

type Body = AcpEntry extends infer E ? (E extends AcpEntry ? Omit<E, "index" | "rev"> : never) : never;

interface Convo {
  entries: AcpEntry[];
  rev: number;
  busy: boolean;
  settings: AcpSetting[];
  used: number;
  /** A turn waiting on a question: what happens once it is answered. */
  after?: () => void;
}

const convos = new Map<string, Convo>();

const SETTINGS: AcpSetting[] = [
  { id: "mode", name: "Mode", category: "mode", current: "default", options: [
    { value: "default", name: "Default", description: "Asks before it edits or runs anything" },
    { value: "acceptEdits", name: "Accept edits", description: "Edits without asking" },
    { value: "plan", name: "Plan", description: "Plans, changes nothing" },
  ] },
  { id: "model", name: "Model", category: "model", current: "sonnet", options: [
    { value: "sonnet", name: "Sonnet", description: null },
    { value: "opus", name: "Opus", description: null },
  ] },
];

function convo(pane: string): Convo {
  let c = convos.get(pane);
  if (!c) {
    c = { entries: [], rev: 0, busy: false, settings: structuredClone(SETTINGS), used: 18_000 };
    convos.set(pane, c);
  }
  return c;
}

function push(c: Convo, body: Body): AcpEntry {
  const e = { ...body, index: c.entries.length, rev: ++c.rev } as AcpEntry;
  c.entries.push(e);
  return e;
}

function change(c: Convo, e: AcpEntry, fields: Partial<Body>) {
  Object.assign(e, fields, { rev: ++c.rev });
}

const tell = (pane: string) => void emit("acp:update", pane);

/** A reply streamed a few words at a time, then the turn ends. */
function reply(pane: string, text: string, done = () => {}) {
  const c = convo(pane);
  const words = text.split(/(?<= )/);
  const e = push(c, { kind: "agent", text: "" });
  let i = 0;
  const step = () => {
    if (!c.busy) return;
    if (i < words.length) {
      change(c, e, { text: (e as { text: string }).text + words[i++] });
      tell(pane);
      setTimeout(step, 60);
      return;
    }
    c.busy = false;
    c.used += 1200;
    done();
    tell(pane);
  };
  step();
}

/** A conversation already under way, for the busy world's ACP pane. */
export function seedAcp(pane: string) {
  const c = convo(pane);
  push(c, { kind: "user", text: "Add rate limiting to POST /login: 5 tries a minute per IP.", queued: false, dropped: false });
  push(c, { kind: "thought", text: "The login handler is in api/src/auth/login.ts. There is already a Redis client in api/src/cache.ts." });
  push(c, { kind: "plan", steps: [
    { content: "Find the login handler", status: "completed" },
    { content: "Add a sliding-window limiter on Redis", status: "in_progress" },
    { content: "Return 429 with Retry-After", status: "pending" },
    { content: "Test it", status: "pending" },
  ] });
  push(c, { kind: "tool", id: "t1", title: "Read api/src/auth/login.ts", tool: "read", status: "completed",
    content: [], locations: ["/Users/you/.villain-worktrees/ACME-123/api/src/auth/login.ts:1"] });
  push(c, { kind: "tool", id: "t2", title: "rg -n \"redis\" api/src", tool: "execute", status: "completed",
    content: [{ type: "text", text: "api/src/cache.ts:3:import { createClient } from \"redis\";\napi/src/cache.ts:9:export const redis = createClient();" }], locations: [] });
  push(c, { kind: "agent", text: "The handler has no limiter yet. I'll add one in **`api/src/auth/limit.ts`** and use it from `login.ts`." });
  push(c, { kind: "tool", id: "t3", title: "Write api/src/auth/limit.ts", tool: "edit", status: "pending",
    content: [{ type: "diff", path: "/Users/you/.villain-worktrees/ACME-123/api/src/auth/limit.ts", old: null,
      new: "import { redis } from \"../cache\";\n\nexport async function allow(ip: string): Promise<boolean> {\n  const key = `login:${ip}`;\n  const n = await redis.incr(key);\n  if (n === 1) await redis.expire(key, 60);\n  return n <= 5;\n}" }],
    locations: [] });
  const q = push(c, { kind: "question", title: "Write api/src/auth/limit.ts", answer: null, options: [
    { id: "allow", name: "Allow", kind: "allow_once" },
    { id: "allow_always", name: "Always allow", kind: "allow_always" },
    { id: "reject", name: "Reject", kind: "reject_once" },
  ] });
  c.busy = true;
  c.used = 64_000;
  c.after = () => {
    const tool = c.entries.find((e) => e.kind === "tool" && e.id === "t3");
    const allowed = c.entries[q.index].kind === "question" && (c.entries[q.index] as { answer: string | null }).answer !== "reject";
    if (tool) change(c, tool, { status: allowed ? "completed" : "failed" });
    reply(pane, allowed
      ? "Written. Next I'll call `allow(ip)` at the top of the handler and answer 429 with `Retry-After: 60`."
      : "Understood, I won't write that file. How would you like the limit kept instead?");
  };
}

export const acpAnswers: Record<string, (a: Record<string, unknown>) => unknown> = {
  acp_view: (a): AcpView => {
    const c = convo(a.paneId as string);
    const since = a.since as number | null;
    const reset = since === null || since > c.rev;
    return {
      rev: c.rev, base: 0, len: c.entries.length, reset,
      entries: structuredClone(c.entries.filter((e) => reset || e.rev > (since ?? 0))),
      agent: "Claude Agent 0.22.2", ready: true, busy: c.busy, queued: 0,
      settings: structuredClone(c.settings),
      commands: [
        { name: "compact", description: "Summarise the conversation to free context", hint: "<instructions>" },
        { name: "context", description: "Show context usage", hint: null },
        { name: "review", description: "Review the changes on this branch", hint: null },
      ],
      usage: { used: c.used, size: 200_000, cost: "0.42 USD" }, exited: false,
    };
  },
  acp_prompt: (a) => {
    const pane = a.paneId as string;
    const c = convo(pane);
    const text = a.text as string;
    push(c, { kind: "user", text, queued: c.busy, dropped: false });
    if (c.busy) {
      tell(pane);
      return null;
    }
    c.busy = true;
    const tool = push(c, { kind: "tool", id: `t${c.rev}`, title: "Read api/src/auth/login.ts", tool: "read", status: "in_progress", content: [], locations: [] });
    tell(pane);
    setTimeout(() => {
      change(c, tool, { status: "completed" });
      if (/write/i.test(text)) {
        push(c, { kind: "question", title: "Write api/src/auth/login.ts", answer: null, options: [
          { id: "allow", name: "Allow", kind: "allow_once" },
          { id: "reject", name: "Reject", kind: "reject_once" },
        ] });
        c.after = () => reply(pane, "Done: the handler now checks the limiter first.");
        tell(pane);
        return;
      }
      reply(pane, "Looked at it. The redirect loop comes from `next` pointing back at `/login` when the session cookie is missing; I'd drop `next` when it is the login page itself.");
    }, 500);
    return null;
  },
  acp_answer: (a) => {
    const pane = a.paneId as string;
    const c = convo(pane);
    const e = c.entries[a.entry as number];
    if (!e || e.kind !== "question" || e.answer) throw new Error("that question is no longer open");
    change(c, e, { answer: a.option as string });
    const next = c.after;
    c.after = undefined;
    next?.();
    tell(pane);
    return null;
  },
  acp_cancel: (a) => {
    const pane = a.paneId as string;
    const c = convo(pane);
    if (!c.busy) return false;
    c.busy = false;
    c.after = undefined;
    for (const e of c.entries) {
      if (e.kind === "question" && !e.answer) change(c, e, { answer: "cancelled" });
      if (e.kind === "user" && e.queued) change(c, e, { queued: false, dropped: true });
    }
    push(c, { kind: "note", text: "Stopped.", error: false });
    tell(pane);
    return true;
  },
  acp_set: (a) => {
    const pane = a.paneId as string;
    const s = convo(pane).settings.find((x) => x.id === a.setting);
    if (s) s.current = a.value as string;
    convo(pane).rev++;
    tell(pane);
    return null;
  },
};
