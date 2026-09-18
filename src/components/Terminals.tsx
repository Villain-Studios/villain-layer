import { useEffect, useMemo, useState } from "react";
import { api } from "../lib/api";
import { useStore } from "../store";
import type { PaneInfo, TaskView } from "../lib/types";
import { TerminalPane } from "./Terminal";
import { Field, Modal } from "./ui";

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
  /** null = the task root, where every repo is visible as a sibling folder. */
  const [scope, setScope] = useState<string | null>(null);

  const multi = task.checkouts.length > 1;

  useEffect(() => { setScope(null); }, [task.id]);

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

  async function launchShell() {
    try {
      const pane = await api.spawnShell(task.id, scope);
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
            <span className={`dot ${p.running ? "live" : "gone"}`} />
            {p.title}
            {!p.running && p.exit_code !== null && (
              <span style={{ color: "var(--dimmer)" }}>({p.exit_code})</span>
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

        {installed.map((a) => (
          <button key={a.id} className="btn btn-sm" onClick={() => setLaunching(a.id)}>
            + {a.name}
          </button>
        ))}
        <button className="btn btn-sm" onClick={launchShell}>+ Shell</button>
      </div>

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

      {launching && (
        <Modal
          title={`Start ${agents.find((a) => a.id === launching)?.name ?? launching}`}
          onClose={() => setLaunching(null)}
          footer={
            <>
              <button className="btn" onClick={() => setLaunching(null)}>Cancel</button>
              <button className="btn btn-primary" onClick={() => void launchAgent(launching)}>
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
            hint="Optional. Leave blank to drop into the agent with no instruction."
          >
            <textarea
              rows={6}
              autoFocus
              value={prompt}
              onChange={(e) => setPrompt(e.target.value)}
              placeholder={
                task.issue_key
                  ? `Implement ${task.issue_key}: ${task.name}`
                  : "What should this agent do?"
              }
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
