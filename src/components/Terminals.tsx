import { useEffect, useMemo, useRef, useState } from "react";
import { api, errMessage } from "../lib/api";
import { read, write } from "../lib/persist";
import { markStopping, useStore } from "../store";
import { paneState } from "../lib/derive";
import type { PaneInfo, Resumable, TaskView } from "../lib/types";
import { TerminalPane } from "./Terminal";
import { ContextMenu, Field, Modal, Spinner } from "./ui";
import type { MenuItem } from "./ui";

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
  const tab = useStore((s) => s.tab);
  const toast = useStore((s) => s.toast);
  const fail = useStore((s) => s.fail);

  const [active, setActive] = useState<string | null>(null);
  const [launching, setLaunching] = useState<string | null>(null);
  const [prompt, setPrompt] = useState("");
  const [promptLoading, setPromptLoading] = useState(false);
  const [handoff, setHandoff] = useState<{ from: PaneInfo; agentId: string } | null>(null);
  const [handoffPrompt, setHandoffPrompt] = useState("");
  const [handoffLoading, setHandoffLoading] = useState(false);
  const [handoffBusy, setHandoffBusy] = useState(false);
  const [stopOld, setStopOld] = useState(true);
  /** Panes whose ✕ was pressed, still waiting on their grace period. */
  const [closing, setClosing] = useState<Set<string>>(new Set());
  /**
   * A start in flight. Spawning takes long enough to click twice, and each
   * click started another agent in the same worktree on the same prompt.
   * A ref as well as state: two clicks in one frame both saw the state false.
   */
  const [spawning, setSpawning] = useState(false);
  const spawnRef = useRef(false);
  async function once(start: () => Promise<void>) {
    if (spawnRef.current) return;
    spawnRef.current = true;
    setSpawning(true);
    try {
      await start();
    } finally {
      spawnRef.current = false;
      setSpawning(false);
    }
  }
  const [resumable, setResumable] = useState<Resumable[]>([]);
  const [addMenu, setAddMenu] = useState<{ x: number; y: number } | null>(null);
  const addRef = useRef<HTMLButtonElement>(null);
  /** null = the task root, where every repo is visible as a sibling folder. */
  const [scope, setScope] = useState<string | null>(null);
  /** Last auto-fetched briefing; if the textarea still matches, scope changes replace it. */
  const fetchedPrompt = useRef("");
  /** Which handoff briefing request is the current one. */
  const handoffAsk = useRef(0);
  const cursorIde = useStore((s) => s.cursorIde);

  const multi = task.checkouts.length > 1;

  useEffect(() => { setScope(null); }, [task.id]);

  // Panes die with the app, but the CLIs keep their own transcripts per
  // directory — so a conversation can be picked up even though the process is
  // long gone. Re-checked when panes change, since starting one creates a
  // transcript and ending one is when you want to resume.
  //
  // Only for a scope that belongs to this task. Before TaskMain was keyed by
  // task, switching rendered once with the last task's repo still picked, and
  // that answer — or a slower earlier one — offered another task's
  // conversations here.
  const scopeHere = scope && task.checkouts.some((c) => c.id === scope) ? scope : null;
  useEffect(() => {
    let current = true;
    api.resumableAgents(task.id, scopeHere)
      .then((r) => { if (current) setResumable(r); })
      .catch(() => { if (current) setResumable([]); });
    return () => { current = false; };
  }, [task.id, scopeHere, panes.length]);

  // Prefill (and refresh on Start-in change) so the text matches where the
  // agent will actually run. Edits the user typed are kept until they change
  // Start in again while the box still holds the previous auto-fetch.
  useEffect(() => {
    if (!launching) return;
    let cancelled = false;
    setPromptLoading(true);
    api.taskPrompt(task.id, scope)
      .then((p) => {
        if (cancelled) return;
        setPrompt((current) => {
          if (!current.trim() || current === fetchedPrompt.current) {
            fetchedPrompt.current = p;
            return p;
          }
          return current;
        });
      })
      .catch((e) => {
        if (!cancelled) toast("error", `Could not load the briefing: ${errMessage(e)}`);
      })
      .finally(() => { if (!cancelled) setPromptLoading(false); });
    return () => { cancelled = true; };
  }, [launching, task.id, scope, toast]);

  // Keep a sensible pane selected as panes come and go. Panes are given fresh
  // ids every launch, so what survives a restart is the position in the bar,
  // not the identity of the pane.
  useEffect(() => {
    if (panes.length === 0) { setActive(null); return; }
    if (!active || !panes.some((p) => p.id === active)) {
      const want = read<number>(`activePane.${task.id}`, panes.length - 1);
      const i = Number.isInteger(want) && want >= 0 && want < panes.length
        ? want
        : panes.length - 1;
      setActive(panes[i].id);
    }
  }, [panes, active, task.id]);

  useEffect(() => {
    const i = panes.findIndex((p) => p.id === active);
    if (i >= 0) write(`activePane.${task.id}`, i);
  }, [active, panes, task.id]);

  // Ctrl+Tab cycles panes the way a browser cycles tabs. It has to be caught in
  // the capture phase: the focused terminal would otherwise take the key and
  // send a literal tab to the process. Bound only while the terminals are the
  // thing on screen — this view stays mounted behind Diff and Pull requests,
  // and switching a pane you cannot see is just a key that appears to do
  // nothing.
  useEffect(() => {
    if (panes.length < 2 || tab !== "terminals") return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Tab" || !e.ctrlKey || e.metaKey || e.altKey) return;
      e.preventDefault();
      e.stopPropagation();
      const at = panes.findIndex((p) => p.id === active);
      const from = at < 0 ? 0 : at;
      const next = (from + (e.shiftKey ? -1 : 1) + panes.length) % panes.length;
      setActive(panes[next].id);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [panes, active, tab]);

  const installed = agents.filter((a) => a.installed);
  const offerResume = resumable.filter(
    (r) => !panes.some((p) => p.running && p.agent_id === r.agent_id),
  );
  const scopeName = scope
    ? task.checkouts.find((c) => c.id === scope)?.project_name ?? "repo"
    : task.checkouts.length > 1
      ? `all ${task.checkouts.length} repos`
      : "the task folder";

  // One "+" rather than a button per CLI: the bar has to stay readable with a
  // pane or two already open, and the list grows with every agent installed.
  const addItems: MenuItem[] = [
    ...offerResume.map((r) => ({
      label: `⟲ Resume ${r.name}${r.last_active ? ` · ${ago(r.last_active)}` : ""}`,
      onSelect: () => void resume(r.agent_id),
    })),
    ...installed.map((a, i) => ({
      label: a.name,
      onSelect: () => setLaunching(a.id),
      separated: i === 0 && offerResume.length > 0,
    })),
    {
      label: "Shell",
      onSelect: () => void launchShell(),
      separated: installed.length > 0 || offerResume.length > 0,
    },
  ];

  const launchAgent = (agentId: string) => once(async () => {
    try {
      const pane = await api.spawnAgent(task.id, agentId, scope, prompt.trim() || null);
      setLaunching(null);
      setPrompt("");
      await refreshPanes();
      setActive(pane.id);
    } catch (e) {
      fail(e);
    }
  });

  const resume = (agentId: string) => once(async () => {
    try {
      const pane = await api.spawnAgent(task.id, agentId, scope, null, true);
      await refreshPanes();
      setActive(pane.id);
    } catch (e) {
      fail(e);
    }
  });

  const launchShell = () => once(async () => {
    try {
      const pane = await api.spawnShell(task.id, scope);
      await refreshPanes();
      setActive(pane.id);
    } catch (e) {
      fail(e);
    }
  });

  /// No CLI can resume another's session, so what moves is the work: the
  /// ticket, the diff, and the outgoing agent's terminal tail.
  function startHandoff(from: PaneInfo) {
    const target = installed.find((a) => a.id !== from.agent_id) ?? installed[0];
    if (!target) {
      fail("No other agent CLI is installed to hand off to.");
      return;
    }
    setHandoff({ from, agentId: target.id });
    setHandoffPrompt("");
    setHandoffLoading(true);
    const asked = ++handoffAsk.current;
    // Only the latest ask lands: a slow briefing for one pane arriving after
    // the dialog was reopened for another replaced what was on screen.
    api.handoffPrompt(from.id)
      .then((p) => { if (asked === handoffAsk.current) setHandoffPrompt(p); })
      .catch((e) => { if (asked === handoffAsk.current) fail(e); })
      .finally(() => { if (asked === handoffAsk.current) setHandoffLoading(false); });
  }

  /// Spawns the new agent, then spends the grace period stopping the old one.
  /// The modal stays up for both, or the press looks like it did nothing while
  /// the outgoing agent saves.
  async function runHandoff() {
    if (!handoff) return;
    setHandoffBusy(true);
    try {
      const pane = await api.spawnAgent(
        task.id,
        handoff.agentId,
        handoff.from.checkout_id,
        handoffPrompt.trim() || null,
      );
      if (stopOld) {
        markStopping(handoff.from.id);
        await api.killPane(handoff.from.id).catch(() => {});
      }
      setHandoff(null);
      setHandoffPrompt("");
      await refreshPanes();
      setActive(pane.id);
    } catch (e) {
      fail(e);
    } finally {
      // Cleared here, where it was set. It lived in launchAgent's finally,
      // so after one handoff the dialog opened already busy, and after a
      // failed one it could not be closed at all — Cancel disabled, and
      // Escape, ✕ and the backdrop all ignored while busy.
      setHandoffBusy(false);
    }
  }

  /// Closing signals the agent and gives it five seconds to save, so the tab
  /// stays put for what feels like a hung click. Marking it spends that time
  /// visibly, and stops a second ✕ landing on a pane already on its way out.
  async function closePane(pane: PaneInfo) {
    if (closing.has(pane.id)) return;
    setClosing((c) => new Set(c).add(pane.id));
    try {
      await api.closePane(pane.id);
      await refreshPanes();
    } catch (e) {
      fail(e);
    } finally {
      setClosing((c) => {
        const next = new Set(c);
        next.delete(pane.id);
        return next;
      });
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
            <span className={`dot ${paneState(p).dot}`} title={paneState(p).label} />
            {p.title}
            {p.notice && (
              <span
                title={p.notice === "trust_prompt" ? "Waiting: trust this folder?" : "Usage limit"}
                style={{ color: "var(--amber)" }}
              >
                {p.notice === "trust_prompt" ? "?" : "⚑"}
              </span>
            )}
            {!p.running && p.exit_code !== null && (
              <span style={{ color: "var(--dimmer)" }}>({p.exit_code})</span>
            )}
            {p.kind === "agent" && installed.length > 0 && !closing.has(p.id) && (
              <span
                className="x"
                title="Hand off to another agent"
                onClick={(e) => { e.stopPropagation(); startHandoff(p); }}
              >
                ⇄
              </span>
            )}
            {closing.has(p.id) ? (
              <Spinner />
            ) : (
              <span className="x" onClick={(e) => { e.stopPropagation(); void closePane(p); }}>
                ✕
              </span>
            )}
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

        <button
          ref={addRef}
          className="btn btn-sm btn-add"
          title="New pane"
          onClick={() =>
            setAddMenu((open) => {
              if (open) return null;
              const r = addRef.current?.getBoundingClientRect();
              return r ? { x: r.right, y: r.bottom + 4 } : null;
            })
          }
        >
          +
        </button>
      </div>

      {addMenu && (
        <ContextMenu
          x={addMenu.x}
          y={addMenu.y}
          ignore={addRef}
          items={addItems}
          onClose={() => setAddMenu(null)}
        />
      )}

      {(() => {
        const p = panes.find((x) => x.id === active && x.notice);
        if (!p) return null;
        if (p.notice === "trust_prompt") {
          return (
            <div className="limit-banner">
              <span>
                <b>{p.title}</b> is asking whether to trust this folder — answer it in
                the terminal below. Every task gets its own worktree, so this is asked
                once per task, and nothing runs until it is answered.
              </span>
            </div>
          );
        }
        return (
          <div className="limit-banner">
            <span>
              <b>{p.title}</b> looks out of budget — it reported hitting a usage limit.
            </span>
            <div className="spacer" />
            <button className="btn btn-sm btn-primary" onClick={() => startHandoff(p)}>
              Hand off to another agent…
            </button>
          </div>
        );
      })()}

      <div className="pane-stack">
        {panes.map((p) => (
          // Visible also means this tab is the one on screen. Terminals stay
          // mounted behind Diff and Pull requests, and a pane fitted while its
          // container had no height keeps those rows: the process draws into a
          // short terminal inside a tall one, which is the black band under a
          // full-screen editor. Toggling this re-fits when the tab comes back.
          <TerminalPane
            key={p.id}
            pane={p}
            visible={p.id === active && tab === "terminals"}
          />
        ))}
        {panes.length === 0 && (
          <div className="empty">
            <h2>No panes yet</h2>
            <p>
              {multi
                ? `Start an agent in the task folder and it sees all ${task.checkouts.length} repos as sibling folders — the right choice when the change spans them.`
                : "Start an agent in the task folder, with the repository as a folder inside it, or open a shell to run the dev server and tests."}
            </p>
            {offerResume.length > 0 && (
              <div className="row" style={{ marginBottom: 6 }}>
                {offerResume.map((r) => (
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
                  No agent CLIs found on your PATH. Install Claude Code, GitHub
                  Copilot CLI, OpenCode or Gemini CLI and reopen the app.
                </span>
              )}
              {installed.map((a) => (
                <button key={a.id} className="btn" onClick={() => setLaunching(a.id)}>
                  Start {a.name}
                </button>
              ))}
              <button className="btn" onClick={() => void launchShell()}>Open shell</button>
              {cursorIde && (
                <button
                  className="btn"
                  title={`Open ${task.root} in Cursor`}
                  onClick={() => void api.openInCursor(task.root).catch(fail)}
                >
                  Open in Cursor
                </button>
              )}
            </div>
          </div>
        )}
      </div>

      {handoff && (
        <Modal
          title={`Hand off from ${handoff.from.title}`}
          wide
          onClose={() => {
            if (handoffBusy) return;
            setHandoff(null);
            setHandoffPrompt("");
          }}
          footer={
            <>
              <button
                className="btn"
                disabled={handoffBusy}
                onClick={() => { setHandoff(null); setHandoffPrompt(""); }}
              >
                Cancel
              </button>
              <button
                className="btn btn-primary"
                disabled={handoffBusy || handoffLoading || !handoffPrompt.trim()}
                onClick={() => void runHandoff()}
              >
                {handoffBusy ? (
                  <span className="btn-busy">
                    <Spinner />
                    {stopOld ? "Starting and stopping…" : "Starting…"}
                  </span>
                ) : (
                  `Start ${agents.find((a) => a.id === handoff.agentId)?.name ?? "agent"}`
                )}
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
          onClose={() => { if (spawning) return; setLaunching(null); setPrompt(""); fetchedPrompt.current = ""; }}
          footer={
            <>
              <button
                className="btn"
                disabled={spawning}
                onClick={() => { setLaunching(null); setPrompt(""); fetchedPrompt.current = ""; }}
              >
                Cancel
              </button>
              <button
                className="btn btn-primary"
                disabled={promptLoading || spawning}
                onClick={() => void launchAgent(launching)}
              >
                {spawning ? "Starting…" : "Launch"}
              </button>
            </>
          }
        >
          <Field
            label="Start in"
            hint="The task folder is where repositories appear, including ones added later. Starting inside one pins the agent to it, and anything added afterwards lands outside where it can see."
          >
            <select
              value={scope ?? ""}
              onChange={(e) => setScope(e.target.value || null)}
            >
              <option value="">
                The task folder
                {task.checkouts.length > 1 ? ` — all ${task.checkouts.length} repos` : ""}
              </option>
              {task.checkouts.map((c) => (
                <option key={c.id} value={c.id}>Only {c.project_name}</option>
              ))}
            </select>
          </Field>
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
              : task.root}</code>
            {` · ${scopeName}`}
          </div>
        </Modal>
      )}
    </>
  );
}
