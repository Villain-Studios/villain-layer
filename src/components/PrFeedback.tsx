import { useEffect, useMemo, useState, type ReactNode } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, errMessage } from "../lib/api";
import { read, write } from "../lib/persist";
import { useStore } from "../store";
import type { CodeLine, FeedbackItem, FeedbackNote, RepoFeedback, TaskView } from "../lib/types";
import { ChevronIcon, ExternalIcon } from "./icons";
import { Modal, Spinner } from "./ui";
import { AgentTargetFields, useAgentTarget } from "./AgentTarget";
import { Markdown } from "./Markdown";

/** One row in the picker, and what it becomes if it is sent. */
interface Entry {
  key: string;
  item: FeedbackItem;
  /** Picked when the list opens. */
  preset: boolean;
  /** Why it was not picked, when that is not obvious. */
  why: string[];
  /** A thread the reviewer accepted: listed last, shut, and never picked for you. */
  resolved: boolean;
}

/**
 * What was sent before, per task, so reopening the picker after the agent has
 * worked through a round does not pick the same comments again.
 *
 * Keyed by URL: a comment's link is its identity, and a check run's link is
 * that run's, so the same check failing again after a push is new.
 */
const SENT_LIMIT = 500;
const sentKey = (taskId: string) => `feedbackSent.${taskId}`;

/** `open` is `list` without its resolved threads: what "All" picks. */
function entries(row: RepoFeedback, sent: Set<string>): { list: Entry[]; open: Entry[]; resolved: number } {
  const out: Entry[] = [];
  const done: Entry[] = [];
  const checkout_id = row.checkout_id;
  const person = (n: FeedbackNote) => !n.bot && n.author !== row.author;

  for (const t of row.threads) {
    const last = t.comments[t.comments.length - 1];
    // By its newest comment, not its first: a thread keyed on where it began
    // stayed "sent before" when the reviewer answered the fix with "still
    // wrong" — the one reply most worth sending.
    const key = `t:${last?.url || t.url}`;
    const why: string[] = [];
    if (sent.has(key)) why.push("sent before");
    if (t.outdated) why.push("outdated");
    if (last && last.author === row.author) why.push("you replied last");
    if (t.comments.every((c) => c.bot)) why.push("bot");
    // A resolved thread is one the reviewer accepted, and sending it invites
    // the agent to redo work already signed off. It stays in sight, though:
    // hidden, it left no way to check what had been settled, or to send one
    // back on purpose.
    if (t.resolved) why.unshift("resolved");
    (t.resolved ? done : out).push({
      key,
      preset: why.length === 0,
      why,
      resolved: t.resolved,
      item: {
        kind: "thread", checkout_id, path: t.path, line: t.line, start_line: t.start_line, code: t.code,
        outdated: t.outdated, resolved: t.resolved, url: t.url,
        comments: t.comments.map((c) => ({ author: c.author, body: c.body })),
      },
    });
  }
  for (const r of row.reviews) {
    const key = `r:${r.url}`;
    const why: string[] = [];
    if (sent.has(key)) why.push("sent before");
    if (!person(r)) why.push(r.bot ? "bot" : "yours");
    out.push({
      key, preset: why.length === 0, why, resolved: false,
      item: { kind: "review", checkout_id, author: r.author, state: r.state, body: r.body, url: r.url },
    });
  }
  for (const c of row.comments) {
    const key = `c:${c.url}`;
    const why: string[] = [];
    if (sent.has(key)) why.push("sent before");
    if (!person(c)) why.push(c.bot ? "bot" : "yours");
    out.push({
      key, preset: why.length === 0, why, resolved: false,
      item: { kind: "comment", checkout_id, author: c.author, body: c.body, url: c.url },
    });
  }
  for (const k of row.checks) {
    const key = `k:${k.url ?? `${checkout_id}:${k.name}`}`;
    const why = sent.has(key) ? ["sent before"] : [];
    out.push({
      key, preset: why.length === 0, why, resolved: false,
      item: {
        kind: "check", checkout_id, name: k.name, conclusion: k.conclusion,
        url: k.url, summary: k.summary, log: k.log,
      },
    });
  }
  return { list: [...out, ...done], open: out, resolved: done.length };
}

const REVIEW_VERB: Record<string, string> = {
  CHANGES_REQUESTED: "requested changes",
  APPROVED: "approved",
  COMMENTED: "reviewed",
};

const OP_CLASS = { "+": "add", "-": "del", " ": "" } as const;

/** The lines a thread is on, above its comments, as GitHub shows them. */
function Code({ lines }: { lines: CodeLine[] }) {
  return (
    <div className="fb-code">
      <div className="diff-lines">
        {lines.map((l, i) => (
          <div key={i} className={`diff-line ${OP_CLASS[l.op] ?? ""}`}>
            <span className="ln">{l.n ?? ""}</span>
            <span className="tx">{l.op}{l.text}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

function EntryRow({ entry, on, onToggle }: { entry: Entry; on: boolean; onToggle: () => void }) {
  const item = entry.item;
  const [open, setOpen] = useState(!entry.resolved);
  let head: string;
  let content: ReactNode;
  // All of it, as GitHub shows it: choosing what to send means reading it,
  // and a two-line preview behind "more" was a click per comment.
  if (item.kind === "thread") {
    const range = item.start_line && item.line && item.start_line < item.line
      ? `${item.start_line}-${item.line}`
      : item.line;
    head = range ? `${item.path}:${range}` : item.path;
    content = (
      <>
        {item.code.length > 0 && <Code lines={item.code} />}
        {item.comments.map((c, n) => (
          <div key={n} className="fb-comment">
            <div className="fb-author">{c.author}</div>
            <Markdown text={c.body} />
          </div>
        ))}
      </>
    );
  } else if (item.kind === "review") {
    head = `${item.author} ${REVIEW_VERB[item.state ?? ""] ?? "reviewed"}`;
    content = item.body && <Markdown text={item.body} />;
  } else if (item.kind === "comment") {
    head = `${item.author} commented`;
    content = item.body && <Markdown text={item.body} />;
  } else {
    head = `${item.name} — ${item.conclusion.replace(/_/g, " ")}`;
    content = (
      <>
        {item.summary && <Markdown text={item.summary} />}
        {item.log && <pre className="fb-log">{item.log}</pre>}
        {!item.summary && !item.log && <div className="fb-body">No report or log could be read.</div>}
      </>
    );
  }
  const url = item.url;

  return (
    <div className="fb-item">
      <input type="checkbox" checked={on} onChange={onToggle} />
      <div className="fb-main">
        {/* The heading picks the item; the body is for reading, selecting and following links. */}
        <div className="row fb-pick" onClick={onToggle}>
          {entry.resolved && (
            <button
              className="btn-sm fb-fold"
              title={open ? "Hide the thread" : "Show the thread"}
              aria-expanded={open}
              onClick={(e) => { e.stopPropagation(); setOpen((o) => !o); }}
            >
              <span className={`chev${open ? " open" : ""}`}><ChevronIcon /></span>
            </button>
          )}
          <span className={item.kind === "thread" || item.kind === "check" ? "fb-head mono" : "fb-head"}>{head}</span>
          {entry.why.map((w) => <span key={w} className="chip">{w}</span>)}
          <div className="spacer" />
          {url && (
            <button
              className="btn-sm"
              title="Open on GitHub"
              onClick={(e) => { e.stopPropagation(); void openUrl(url).catch(() => {}); }}
            >
              <ExternalIcon />
            </button>
          )}
        </div>
        {open && content && <div className="fb-content">{content}</div>}
      </div>
    </div>
  );
}

/**
 * What reviewers and CI said on a task's open pull requests, picked through
 * and handed to an agent in one go.
 *
 * The same move as notes in the Diff tab, for feedback that did not come from
 * you: copying eight inline comments and a failing log out of the browser was
 * the step between a review landing and the agent starting on it.
 */
export function PrFeedback({ task, onClose }: { task: TaskView; onClose: () => void }) {
  const toast = useStore((s) => s.toast);
  const fail = useStore((s) => s.fail);
  const at = useAgentTarget(task);

  const [rows, setRows] = useState<RepoFeedback[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);

  const sent = useMemo(() => new Set(read<string[]>(sentKey(task.id), [])), [task.id]);
  const built = useMemo(
    () => (rows ?? []).map((row) => ({ row, ...entries(row, sent) })),
    [rows, sent],
  );

  useEffect(() => {
    let stop = false;
    setRows(null);
    setError(null);
    api.githubPrFeedback(task.id)
      .then((r) => {
        if (stop) return;
        setRows(r);
        const next = new Set<string>();
        for (const row of r) for (const e of entries(row, sent).list) if (e.preset) next.add(e.key);
        setPicked(next);
      })
      .catch((e) => { if (!stop) setError(errMessage(e)); });
    return () => { stop = true; };
  }, [task.id, sent]);

  const chosen = built.flatMap((b) => b.list.filter((e) => picked.has(e.key)));
  const total = built.reduce((n, b) => n + b.list.length, 0);
  const resolvedTotal = built.reduce((n, b) => n + b.resolved, 0);

  function toggle(set: Set<string>, key: string): Set<string> {
    const next = new Set(set);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    return next;
  }

  async function send() {
    if (chosen.length === 0) return;
    setBusy(true);
    try {
      const items = chosen.map((e) => e.item);
      const who = await at.send((paneId, scope) => api.sendPrFeedback(task.id, paneId, scope, items));
      toast("success", `Sent ${chosen.length} item${chosen.length === 1 ? "" : "s"} of feedback to ${who}.`);
      const keep = [...new Set([...read<string[]>(sentKey(task.id), []), ...chosen.map((e) => e.key)])];
      write(sentKey(task.id), keep.slice(-SENT_LIMIT));
      onClose();
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  const noAgent = at.running.length === 0;
  const footer = (
    <>
      {rows && total > 0 && <AgentTargetFields task={task} at={at} disabled={busy} />}
      <div className="spacer" />
      <button className="btn" onClick={onClose} disabled={busy}>Cancel</button>
      <button
        className="btn btn-primary"
        disabled={busy || chosen.length === 0 || at.stuck}
        title={at.stuck ? "Install an agent CLI first" : undefined}
        onClick={() => void send()}
      >
        {busy
          ? "Sending…"
          : `${noAgent ? "Start & send" : "Send"} ${chosen.length || ""}`.trim()}
      </button>
    </>
  );

  return (
    <Modal title="Feedback on the pull requests" wide tall onClose={() => !busy && onClose()} footer={footer}>
      {error && <div className="muted" style={{ color: "var(--red)" }}>{error}</div>}
      {!rows && !error && (
        <div className="row muted"><Spinner /> Reading reviews and checks…</div>
      )}
      {rows && rows.length === 0 && (
        <div className="muted">No repository in this task has an open pull request.</div>
      )}
      {rows && rows.length > 0 && total === resolvedTotal && !rows.some((r) => r.error) && (
        <div className="muted" style={{ lineHeight: 1.6 }}>
          Nothing to answer: no open review threads, comments or failing checks
          {resolvedTotal > 0 ? " — every thread has been resolved" : ""}.
        </div>
      )}
      {built.map(({ row, list, open }) => (
        <div key={row.checkout_id} className="fb-repo">
          <div className="row" style={{ marginBottom: 6 }}>
            <h3 style={{ margin: 0, fontSize: 13.5 }}>
              <span style={{ color: "var(--dim)" }}>{row.repo}</span>
              {row.number > 0 && ` #${row.number} ${row.title}`}
            </h3>
            <div className="spacer" />
            {open.length > 1 && (
              <button
                className="btn-sm"
                onClick={() => {
                  const all = open.every((e) => picked.has(e.key));
                  setPicked((p) => {
                    const next = new Set(p);
                    for (const e of open) {
                      if (all) next.delete(e.key);
                      else next.add(e.key);
                    }
                    return next;
                  });
                }}
              >
                {open.every((e) => picked.has(e.key)) ? "None" : "All"}
              </button>
            )}
          </div>
          {row.error && <div className="muted" style={{ color: "var(--red)" }}>{row.error}</div>}
          {list.map((e) => (
            <EntryRow
              key={e.key}
              entry={e}
              on={picked.has(e.key)}
              onToggle={() => setPicked((p) => toggle(p, e.key))}
            />
          ))}
          {!row.error && list.length === 0 && (
            <div className="muted fb-note">Nothing said here yet, and no check is failing.</div>
          )}
        </div>
      ))}
      {rows && total > 0 && (
        <div className="muted fb-note" style={{ marginTop: 10, lineHeight: 1.5 }}>
          The picked items are written to a file in the task folder, and the agent is told to
          work through it and say what it did about each. Nothing is posted to GitHub.
        </div>
      )}
    </Modal>
  );
}
