import { useEffect, useMemo, useState } from "react";
import { api } from "../lib/api";
import { useStore } from "../store";
import type { PaneInfo, Resumable, TaskView } from "../lib/types";
import { TerminalPane } from "./Terminal";
import { Field, Modal } from "./ui";

function ago(unixSeconds: number): string {
  const s = Math.max(0, Math.round(Date.now() / 1000 - unixSeconds));
  if (s < 90) return "just now";
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  if (s < 86400) return `${Math.round(s / 3600)}h ago`;
  return `${Math.round(s / 86400)}d ago`;
}

export function Terminals({ task }: { task: TaskView }) {
  const allPanes = useStore((s) => s.panes);
  const panes = useMemo(
    () => allPanes.filter((p) => p.task_id === task.id),
    [allPanes, task.id],
  );
  const agents = useStore((s) => s.agents);
  const refreshPanes = useStore((s) => s.refreshPanes);
  const fail = useStore((s) => s.fail);

  const [active, setActive] = useState<string | null>(null);
  const [launching, setLaunching] = useState<string | null>(null);
  const [prompt, setPrompt] = useState("");
  const [promptLoading, setPromptLoading] = useState(false);
  const [handoff, setHandoff] = useState<{ from: PaneInfo; agentId: string } | null>(null);
  const [handoffPrompt, setHandoffPrompt] = useState("");
  const [handoffLoading, setHandoffLoading] = useState(false);
  const [stopOld, setStopOld] = useState(true);
  const [resumable, setResumable] = useState<Resumable[]>([]);
  /** null = the task root, where every repo is visible as a sibling folder. */
  const [scope, setScope] = useState<string | null>(null);

  const multi = task.checkouts.length > 1;

  useEffect(() => { setScope(null); }, [task.id]);

  // Panes die with the app, but the CLIs keep their own transcripts per
  // directory — so a conversation can be picked up even though the process is
  // long gone. Re-checked when panes change, since starting one creates a
  // transcript and ending one is when you want to resume.
  useEffect(() => {
    api.resumableAgents(task.id, scope)
      .then(setResumable)
      .catch(() => setResumable([]));
  }, [task.id, scope, panes.length]);

  // Prefill the real prompt rather than showing one as placeholder text: what
  // is on screen should be what gets sent. For a ticket-backed task this is the
  // same briefing the first agent got, description and repo layout included.
  useEffect(() => {
    if (!launching) return;
    setPromptLoading(true);
    api.taskPrompt(task.id)
      .then((p) => setPrompt((current) => (current.trim() ? current : p)))
      .catch(() => {})
      .finally(() => setPromptLoading(false));
  }, [launching, task.id]);

  // Keep a sensible pane selected as panes come and go.
  useEffect(() => {
    if (panes.length === 0) { setActive(null); return; }
    if (!active || !panes.some((p) => p.id === active)) {
      setActive(panes[panes.length - 1].id);
    }
  }, [panes, active]);

  const installed = agents.filter((a) => a.installed);
  const scopeName = scope
    ? task.checkouts.find((c) => c.id === scope)?.project_name ?? "repo"
    : `all ${task.checkouts.length} repos`;

  async function launchAgent(agentId: string) {
    try {
      const pane = await api.spawnAgent(task.id, agentId, scope, prompt.trim() || null);
      setLaunching(null);
      setPrompt("");
      await refreshPanes();
      setActive(pane.id);
    } catch (e) {
      fail(e);
    }
  }

  async function resume(agentId: string) {
    try {
      const pane = await api.spawnAgent(task.id, agentId, scope, null, true);
      await refreshPanes();
      setActive(pane.id);
    } catch (e) {
      fail(e);
    }
  }

  async function launchShell() {
    try {
      const pane = await api.spawnShell(task.id, scope);
      await refreshPanes();
      setActive(pane.id);
    } catch (e) {
      fail(e);
    }
  }

  /// No CLI can resume another's session, so what moves is the work: the
  /// ticket, the diff, and the outgoing agent's terminal tail.
  function startHandoff(from: PaneInfo) {
    const target = installed.find((a) => a.id !== from.agent_id) ?? installed[0];
    if (!target) {
      fail("No other agent CLI is installed to hand off to.");
      return;
    }
    setHandoff({ from, agentId: target.id });
    setHandoffLoading(true);
    api.handoffPrompt(from.id)
      .then(setHandoffPrompt)
      .catch(fail)
      .finally(() => setHandoffLoading(false));
  }

  async function runHandoff() {
    if (!handoff) return;
    try {
      const pane = await api.spawnAgent(
        task.id,
        handoff.agentId,
        handoff.from.checkout_id,
        handoffPrompt.trim() || null,
      );
      if (stopOld) await api.killPane(handoff.from.id).catch(() => {});
      setHandoff(null);
      setHandoffPrompt("");
      await refreshPanes();
      setActive(pane.id);
    } catch (e) {
      fail(e);
    }
  }

  async function closePane(pane: PaneInfo) {
    try {
      await api.closePane(pane.id);
      await refreshPanes();
    } catch (e) {
      fail(e);
    }
  }

  return (
    <>
      <div className="pane-bar">
        {panes.map((p) => (
          <div
            key={p.id}
            className={`pane-tab${p.id === active ? " active" : ""}`}
            onClick={() => setActive(p.id)}
          >
            <span
              className={`dot ${p.limit_reached && p.running ? "idle" : p.running ? "live" : "gone"}`}
            />
            {p.title}
            {p.limit_reached && (
              <span title="Reported a usage limit" style={{ color: "var(--amber)" }}>
                ⚑
              </span>
            )}
            {!p.running && p.exit_code !== null && (
              <span style={{ color: "var(--dimmer)" }}>({p.exit_code})</span>
            )}
            {p.kind === "agent" && installed.length > 0 && (
              <span
                className="x"
                title="Hand off to another agent"
                onClick={(e) => { e.stopPropagation(); startHandoff(p); }}
              >
                ⇄
              </span>
            )}
            <span className="x" onClick={(e) => { e.stopPropagation(); void closePane(p); }}>
              ✕
            </span>
          </div>
        ))}
        <div className="spacer" />

        {multi && (
          <>
            <span
              className={`repo-tag${scope === null ? " active" : ""}`}
              title="New panes start at the task root, seeing every repo"
              onClick={() => setScope(null)}
            >
              all
            </span>
            {task.checkouts.map((c) => (
              <span
                key={c.id}
                className={`repo-tag${scope === c.id ? " active" : ""}`}
                title={`New panes start inside ${c.project_name}`}
                onClick={() => setScope(c.id)}
              >
                {c.project_name}
              </span>
            ))}
            <span style={{ width: 6 }} />
          </>
        )}

        {resumable.map((r) => (
          <button
            key={`resume-${r.agent_id}`}
            className="btn btn-sm"
            title={`${r.sessions} saved conversation${r.sessions === 1 ? "" : "s"} in this folder`}
            onClick={() => void resume(r.agent_id)}
          >
            ⟲ Resume {r.name}
          </button>
        ))}
        {installed.map((a) => (
          <button key={a.id} className="btn btn-sm" onClick={() => setLaunching(a.id)}>
            + {a.name}
          </button>
        ))}
        <button className="btn btn-sm" onClick={launchShell}>+ Shell</button>
      </div>

      {(() => {
        const stuck = panes.find((p) => p.id === active && p.limit_reached);
        return stuck ? (
          <div className="limit-banner">
            <span>
              <b>{stuck.title}</b> looks out of budget — it reported hitting a usage
              limit.
            </span>
            <div className="spacer" />
            <button className="btn btn-sm btn-primary" onClick={() => startHandoff(stuck)}>
              Hand off to another agent…
            </button>
          </div>
        ) : null;
      })()}

      <div className="pane-stack">
        {panes.map((p) => (
          <TerminalPane key={p.id} pane={p} visible={p.id === active} />
        ))}
        {panes.length === 0 && (
          <div className="empty">
            <h2>No panes yet</h2>
            <p>
              {multi
                ? `Start an agent at the task root and it sees all ${task.checkouts.length} repos as sibling folders — the right choice when the change spans them.`
                : "Start an agent in this worktree, or open a shell to run the dev server and tests beside it."}
            </p>
            {resumable.length > 0 && (
              <div className="row" style={{ marginBottom: 6 }}>
                {resumable.map((r) => (
                  <button
                    key={r.agent_id}
                    className="btn btn-primary"
                    onClick={() => void resume(r.agent_id)}
                  >
                    ⟲ Resume {r.name}
                    {r.last_active && (
                      <span style={{ opacity: 0.75, marginLeft: 6 }}>
                        · {ago(r.last_active)}
                      </span>
                    )}
                  </button>
                ))}
              </div>
            )}
            <div className="row">
              {installed.length === 0 && (
                <span>
                  No agent CLIs found on your PATH. Install Claude Code, Codex or
                  Gemini CLI and reopen the app.
                </span>
              )}
              {installed.map((a) => (
                <button key={a.id} className="btn" onClick={() => setLaunching(a.id)}>
                  Start {a.name}
                </button>
              ))}
              <button className="btn" onClick={launchShell}>Open shell</button>
            </div>
          </div>
        )}
      </div>

      {handoff && (
        <Modal
          title={`Hand off from ${handoff.from.title}`}
          wide
          onClose={() => { setHandoff(null); setHandoffPrompt(""); }}
          footer={
            <>
              <button
                className="btn"
                onClick={() => { setHandoff(null); setHandoffPrompt(""); }}
              >
                Cancel
              </button>
              <button
                className="btn btn-primary"
                disabled={handoffLoading || !handoffPrompt.trim()}
                onClick={() => void runHandoff()}
              >
                Start {agents.find((a) => a.id === handoff.agentId)?.name ?? "agent"}
              </button>
            </>
          }
        >
          <div className="muted" style={{ marginBottom: 12, lineHeight: 1.6 }}>
            No agent CLI can resume another's session, so this carries the work rather
            than the conversation: the ticket, what has changed, and the tail of{" "}
            {handoff.from.title}'s terminal.
          </div>

          <Field label="Continue with">
            <select
              value={handoff.agentId}
              onChange={(e) => setHandoff({ ...handoff, agentId: e.target.value })}
            >
              {installed.map((a) => (
                <option key={a.id} value={a.id}>
                  {a.name}{a.id === handoff.from.agent_id ? " (same agent)" : ""}
                </option>
              ))}
            </select>
          </Field>

          <Field
            label="Handoff briefing"
            hint={handoffLoading ? "Gathering the diff and terminal…" : "Edit freely before sending."}
          >
            <textarea
              rows={14}
              value={handoffPrompt}
              onChange={(e) => setHandoffPrompt(e.target.value)}
            />
          </Field>

          <label className="row" style={{ gap: 7, cursor: "pointer" }}>
            <input
              type="checkbox"
              style={{ width: "auto" }}
              checked={stopOld}
              onChange={(e) => setStopOld(e.target.checked)}
            />
            Stop {handoff.from.title} once the new agent starts
          </label>
        </Modal>
      )}

      {launching && (
        <Modal
          title={`Start ${agents.find((a) => a.id === launching)?.name ?? launching}`}
          onClose={() => { setLaunching(null); setPrompt(""); }}
          footer={
            <>
              <button
                className="btn"
                onClick={() => { setLaunching(null); setPrompt(""); }}
              >
                Cancel
              </button>
              <button
                className="btn btn-primary"
                disabled={promptLoading}
                onClick={() => void launchAgent(launching)}
              >
                Launch
              </button>
            </>
          }
        >
          {multi && (
            <Field
              label="Scope"
              hint="At the task root the agent can read and edit across every repo. Inside one repo it keeps its normal git awareness."
            >
              <select
                value={scope ?? ""}
                onChange={(e) => setScope(e.target.value || null)}
              >
                <option value="">Task root — all {task.checkouts.length} repos</option>
                {task.checkouts.map((c) => (
                  <option key={c.id} value={c.id}>Only {c.project_name}</option>
                ))}
              </select>
            </Field>
          )}
          <Field
            label="Opening prompt"
            hint={
              promptLoading
                ? "Fetching the ticket…"
                : "Sent to the agent as it starts. Edit it, or clear it to drop in with no instruction."
            }
          >
            <textarea
              rows={10}
              autoFocus
              value={prompt}
              onChange={(e) => setPrompt(e.target.value)}
              placeholder="What should this agent do?"
            />
          </Field>
          <div className="muted" style={{ fontSize: 11 }}>
            Starts in <code>{scope
              ? task.checkouts.find((c) => c.id === scope)?.path
              : multi ? task.root : task.checkouts[0]?.path}</code>
            {multi && ` · ${scopeName}`}
          </div>
        </Modal>
      )}
    </>
  );
}
