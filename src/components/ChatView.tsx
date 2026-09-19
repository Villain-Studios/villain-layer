import { useEffect, useMemo, useState } from "react";
import { api } from "../lib/api";
import { CHAT_TASK_ID, useStore } from "../store";
import { TerminalPane } from "./Terminal";

/**
 * A standing agent with no worktree: for questions, drafting tickets, and
 * pulling context from whatever MCP servers the user's own CLI is configured
 * with. It runs in a scratch folder so it can keep notes between sessions.
 */
export function ChatView() {
  const allPanes = useStore((s) => s.panes);
  const panes = useMemo(
    () => allPanes.filter((p) => p.task_id === CHAT_TASK_ID),
    [allPanes],
  );
  const agents = useStore((s) => s.agents);
  const refreshPanes = useStore((s) => s.refreshPanes);
  const fail = useStore((s) => s.fail);

  const [active, setActive] = useState<string | null>(null);
  const installed = agents.filter((a) => a.installed);

  useEffect(() => {
    if (panes.length === 0) { setActive(null); return; }
    if (!active || !panes.some((p) => p.id === active)) {
      setActive(panes[panes.length - 1].id);
    }
  }, [panes, active]);

  async function start(agentId: string) {
    try {
      const pane = await api.spawnChat(agentId);
      await refreshPanes();
      setActive(pane.id);
    } catch (e) {
      fail(e);
    }
  }

  async function close(id: string) {
    try {
      await api.closePane(id);
      await refreshPanes();
    } catch (e) {
      fail(e);
    }
  }

  return (
    <div className="chat">
      <div className="pane-bar">
        {panes.map((p) => (
          <div
            key={p.id}
            className={`pane-tab${p.id === active ? " active" : ""}`}
            onClick={() => setActive(p.id)}
          >
            <span className={`dot ${p.running ? "live" : "gone"}`} />
            {p.title}
            <span className="x" onClick={(e) => { e.stopPropagation(); void close(p.id); }}>✕</span>
          </div>
        ))}
        <div className="spacer" />
        {installed.map((a) => (
          <button key={a.id} className="btn btn-sm" onClick={() => void start(a.id)}>
            + {a.name}
          </button>
        ))}
      </div>

      <div className="pane-stack">
        {panes.map((p) => (
          <TerminalPane key={p.id} pane={p} visible={p.id === active} />
        ))}
        {panes.length === 0 && (
          <div className="empty">
            <h2>Chat</h2>
            <p>
              An agent with no worktree attached — for asking questions, drafting a
              ticket before it exists, or pulling context together from elsewhere.
            </p>
            <p style={{ color: "var(--dimmer)" }}>
              It starts with a <code>CLAUDE.md</code> describing your repos, the tasks
              in flight and which integrations the app is connected to — so it knows
              what you are working on without being told.
            </p>
            <p style={{ color: "var(--dimmer)" }}>
              Reaching Jira, Slack or GitHub is separate: it uses your own CLI's MCP
              servers, not the app's stored credentials.
            </p>
            <div className="row">
              {installed.length === 0 && <span>No agent CLIs found on your PATH.</span>}
              {installed.map((a) => (
                <button key={a.id} className="btn btn-primary" onClick={() => void start(a.id)}>
                  Start {a.name}
                </button>
              ))}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
