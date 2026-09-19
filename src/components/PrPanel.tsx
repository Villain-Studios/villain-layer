import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api } from "../lib/api";
import { useStore } from "../store";
import type { CheckoutPr, CheckRun, RepoResult, TaskView } from "../lib/types";
import { Field, Spinner } from "./ui";

function checkColor(c: CheckRun) {
  if (c.status !== "completed") return "var(--amber)";
  if (c.conclusion === "success") return "var(--green)";
  if (c.conclusion === "skipped" || c.conclusion === "neutral") return "var(--dim)";
  return "var(--red)";
}

export function PrPanel({ task }: { task: TaskView }) {
  const settings = useStore((s) => s.settings);
  const allPanes = useStore((s) => s.panes);
  const agentPanes = useMemo(
    () => allPanes.filter((p) => p.task_id === task.id && p.kind === "agent" && p.running),
    [allPanes, task.id],
  );
  const toggleSettings = useStore((s) => s.toggleSettings);
  const toast = useStore((s) => s.toast);
  const fail = useStore((s) => s.fail);

  const [rows, setRows] = useState<CheckoutPr[]>([]);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState(false);
  const [title, setTitle] = useState(task.name);
  const [body, setBody] = useState("");
  const [draft, setDraft] = useState(true);
  const [drafting, setDrafting] = useState(false);
  const pollRef = useRef<number | null>(null);

  const load = useCallback(async () => {
    if (!settings?.github_connected) return;
    setLoading(true);
    try {
      setRows(await api.githubTaskPrs(task.id));
    } catch (e) {
      fail(e);
    } finally {
      setLoading(false);
    }
  }, [task.id, settings?.github_connected, fail]);

  useEffect(() => { void load(); }, [load]);

  // Stop waiting for a draft if the panel goes away.
  useEffect(() => () => { if (pollRef.current) window.clearInterval(pollRef.current); }, []);

  useEffect(() => {
    setTitle(task.name);
    setBody(task.issue_url ? `Jira: ${task.issue_url}\n` : "");
  }, [task.id, task.name, task.issue_url]);

  function report(results: RepoResult[], verb: string) {
    const bad = results.filter((r) => !r.ok);
    const ok = results.filter((r) => r.ok);
    if (bad.length) toast("error", bad.map((r) => `${r.repo}: ${r.detail}`).join("\n"));
    if (ok.length) {
      toast("success", `${verb} ${ok.map((r) => `${r.repo} (${r.detail})`).join(", ")}`);
    }
  }

  /// The agent that did the work knows what the diff cannot say: what it tried,
  /// what it left out, where a reviewer should look hardest. Ask it, rather than
  /// making the reviewer reconstruct that from the changes.
  async function draftWithAgent() {
    const pane = agentPanes[0];
    if (!pane) {
      toast("error", "No running agent in this task. Start one in Terminals first.");
      return;
    }
    try {
      await api.requestPrDescription(task.id, pane.id);
      setDrafting(true);
      toast("info", `Asked ${pane.title} to write the description — watch it work in Terminals.`);

      const started = Date.now();
      if (pollRef.current) window.clearInterval(pollRef.current);
      pollRef.current = window.setInterval(async () => {
        const text = await api.takePrDescription(task.id).catch(() => null);
        if (text) {
          setBody(text.trim());
          setDrafting(false);
          if (pollRef.current) window.clearInterval(pollRef.current);
          toast("success", "Description drafted — edit it before opening the PR.");
        } else if (Date.now() - started > 5 * 60 * 1000) {
          setDrafting(false);
          if (pollRef.current) window.clearInterval(pollRef.current);
          toast("error", "Gave up waiting for the draft. The agent may still be working.");
        }
      }, 2000);
    } catch (e) {
      setDrafting(false);
      fail(e);
    }
  }

  async function push() {
    setBusy(true);
    try {
      report(await api.pushTask(task.id), "Pushed");
      await load();
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  async function openPrs() {
    setBusy(true);
    try {
      report(await api.githubOpenPrs(task.id, title.trim(), body, draft), "Opened");
      await load();
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  if (!settings?.github_connected) {
    return (
      <div className="empty">
        <h2>GitHub not connected</h2>
        <p>Connect GitHub or GitHub Enterprise to open pull requests and read CI from here.</p>
        <button className="btn" onClick={() => toggleSettings(true)}>Open settings</button>
      </div>
    );
  }

  const pending = rows.filter((r) => !r.pr && r.changed > 0);
  const open = rows.filter((r) => r.pr);

  return (
    <div className="panel-scroll">
      {loading && <div className="row" style={{ marginBottom: 10 }}><Spinner /> Loading…</div>}

      {open.map((row) => (
        <div key={row.checkout_id} className="card">
          <div className="row">
            <h3 style={{ margin: 0 }}>
              <span style={{ color: "var(--dim)" }}>{row.repo}</span>{" "}
              #{row.pr!.number} {row.pr!.title}
            </h3>
            <div className="spacer" />
            {row.pr!.draft && <span className="chip">draft</span>}
            <span className="chip">{row.pr!.state}</span>
          </div>
          <div className="muted" style={{ marginTop: 6 }}>
            {row.pr!.head} → {row.pr!.base} · opened by {row.pr!.author}
          </div>

          {row.checks.length > 0 && (
            <div style={{ marginTop: 10 }}>
              {row.checks.map((c) => (
                <div key={c.name + c.status} className="check">
                  <span className="dot" style={{ background: checkColor(c) }} />
                  <span className="name">{c.name}</span>
                  <span className="muted">{c.conclusion ?? c.status}</span>
                  {c.url && <button className="btn-sm" onClick={() => void openUrl(c.url!)}>↗</button>}
                </div>
              ))}
            </div>
          )}

          <div className="row" style={{ marginTop: 10 }}>
            <button className="btn btn-sm" onClick={() => void openUrl(row.pr!.url)}>
              Open on GitHub
            </button>
          </div>
        </div>
      ))}

      {rows.some((r) => r.error) && (
        <div className="card">
          <h3>Repos that could not be read</h3>
          {rows.filter((r) => r.error).map((r) => (
            <div key={r.checkout_id} className="check">
              <span className="dot" style={{ background: "var(--red)" }} />
              <span className="name">{r.repo}</span>
              <span className="muted">{r.error}</span>
            </div>
          ))}
        </div>
      )}

      <div className="card">
        <h3>
          {pending.length === 0 && open.length > 0
            ? "Everything with changes has a PR"
            : `Open pull request${pending.length === 1 ? "" : "s"}`}
        </h3>

        {rows.length > 0 && (
          <div className="muted" style={{ marginBottom: 10, lineHeight: 1.6 }}>
            {rows.map((r) => (
              <div key={r.checkout_id} className="row">
                <span className="dot" style={{
                  background: r.pr ? "var(--green)" : r.changed > 0 ? "var(--amber)" : "var(--dimmer)",
                }} />
                <span style={{ fontFamily: "var(--mono)", fontSize: 11.5, width: 140 }}>
                  {r.repo}
                </span>
                <span>
                  {r.pr ? `PR #${r.pr.number}` : r.changed > 0
                    ? `${r.changed} changed file${r.changed === 1 ? "" : "s"}`
                    : "no changes"}
                </span>
              </div>
            ))}
          </div>
        )}

        {pending.length > 0 && (
          <>
            <Field label="Title">
              <input value={title} onChange={(e) => setTitle(e.target.value)} />
            </Field>
            <Field
              label="Description"
              hint={
                drafting
                  ? "Waiting for the agent to save its draft…"
                  : "Let the agent that did the work write this — it knows what the diff does not."
              }
            >
              <textarea rows={8} value={body} onChange={(e) => setBody(e.target.value)} />
              <div className="row" style={{ marginTop: 8 }}>
                <button
                  className="btn btn-sm"
                  disabled={drafting || agentPanes.length === 0}
                  title={
                    agentPanes.length === 0
                      ? "Start an agent in Terminals first"
                      : `Ask ${agentPanes[0].title} to write it`
                  }
                  onClick={() => void draftWithAgent()}
                >
                  {drafting ? "Drafting…" : "✨ Draft with agent"}
                </button>
                {drafting && <Spinner />}
              </div>
            </Field>
            <div className="row" style={{ marginBottom: 12 }}>
              <label className="row" style={{ gap: 6, cursor: "pointer" }}>
                <input
                  type="checkbox"
                  style={{ width: "auto" }}
                  checked={draft}
                  onChange={(e) => setDraft(e.target.checked)}
                />
                Open as draft
              </label>
            </div>
          </>
        )}

        <div className="row">
          <button className="btn" disabled={busy} onClick={() => void push()}>
            Push all
          </button>
          <button className="btn btn-sm" onClick={() => void load()}>Refresh</button>
          <div className="spacer" />
          <button
            className="btn btn-primary"
            disabled={busy || pending.length === 0 || !title.trim()}
            onClick={() => void openPrs()}
          >
            {busy
              ? "Working…"
              : `Push & open ${pending.length} PR${pending.length === 1 ? "" : "s"}`}
          </button>
        </div>

        <div className="muted" style={{ marginTop: 10, lineHeight: 1.55 }}>
          Opens one PR per repo with changes, all from <code>{task.branch}</code>.
          {task.issue_key
            ? ` The links are then posted back to ${task.issue_key} as a single comment, so the ticket is where the set stays joined up.`
            : ""}
        </div>
      </div>
    </div>
  );
}
