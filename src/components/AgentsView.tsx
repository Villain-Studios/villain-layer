import { useState } from "react";

import { api } from "../lib/api";
import { ago } from "../lib/time";
import { CHAT_TASK_ID, markStopping, needsYou, paneState, useNow, useStore } from "../store";
import { SidebarToggle, Spinner } from "./ui";

export function AgentsView() {
  const panes = useStore((s) => s.panes);
  const tasks = useStore((s) => s.tasks);
  const agents = useStore((s) => s.agents);
  const refreshPanes = useStore((s) => s.refreshPanes);
  const select = useStore((s) => s.select);
  const setView = useStore((s) => s.setView);
  const fail = useStore((s) => s.fail);
  const now = useNow(5_000);

  /// Stopping asks the agent to exit and waits up to five seconds for it to
  /// save, so the row does not change until then. Say which one is stopping.
  /// Several at once: one id meant stopping B while A was still going gave
  /// A its button back, and A finishing cleared B's spinner.
  const [stopping, setStopping] = useState<Set<string>>(new Set());

  async function stop(paneId: string) {
    if (stopping.has(paneId)) return;
    setStopping((s) => new Set(s).add(paneId));
    markStopping(paneId);
    try {
      await api.killPane(paneId);
      await refreshPanes();
    } catch (e) {
      fail(e);
    } finally {
      setStopping((s) => {
        const next = new Set(s);
        next.delete(paneId);
        return next;
      });
    }
  }

  // What needs you first, so the dock's count is the top of this list.
  const working = panes
    .filter((p) => p.kind === "agent")
    .sort((a, b) => Number(needsYou(b)) - Number(needsYou(a)));
  const live = working.filter((p) => p.running);
  const waiting = working.filter(needsYou).length;

  return (
    <div className="wide">
      <div className="wide-head">
        <SidebarToggle />
        <h2>Agents</h2>
        <span className="sub">
          {waiting > 0 && (
            <span style={{ color: "var(--amber)" }}>{waiting} need{waiting === 1 ? "s" : ""} you · </span>
          )}
          {live.length} running · {working.length - live.length} exited — pick a task
          on the left to work on one
        </span>
        <div className="spacer" />
        <button className="btn btn-sm" onClick={() => void refreshPanes()}>Refresh</button>
      </div>

      {working.length === 0 && (
        <div className="card">
          <div className="muted" style={{ lineHeight: 1.6 }}>
            Nothing running yet. Pick a task on the left, create one with <b>+</b>, or
            open the Tickets tab and start straight from a Jira issue. A task can span
            as many repositories as the change needs.
          </div>
        </div>
      )}

      {working.map((pane) => {
        const task = tasks.find((t) => t.id === pane.task_id);
        const isChat = pane.task_id === CHAT_TASK_ID;
        const st = paneState(pane);
        const agent = agents.find((a) => a.id === pane.agent_id);

        return (
          <div key={pane.id} className="agent-card">
            <span className={`dot ${st.dot}`} />
            <div className="who">
              <div className="name">
                {agent?.name ?? pane.title}
                {pane.notice && (
                  <span style={{ color: "var(--amber)", marginLeft: 6 }} title={pane.notice}>
                    {pane.notice === "trust_prompt" ? "?" : "⚑"}
                  </span>
                )}
              </div>
              <div
                className="where"
                style={pane.notice || needsYou(pane) ? { color: "var(--amber)" } : undefined}
              >
                {st.label}
              </div>
            </div>

            <div className="ctx">
              <div className="task">
                {isChat ? "Chat" : task?.name ?? "(task removed)"}
              </div>
              <div className="branch">
                {isChat ? pane.cwd : `${task?.branch ?? "?"} · ${pane.title.split(" · ")[1] ?? ""}`}
              </div>
            </div>

            <span className="when" title="Since it started doing what it is doing now">
              {ago(pane.activity_since, now)}
            </span>

            <button
              className="btn btn-sm"
              onClick={() => {
                if (isChat) setView("chat");
                else if (task) select(task.id);
              }}
            >
              Open
            </button>
            {pane.running && (
              <button
                className="btn btn-sm btn-danger"
                disabled={stopping.has(pane.id)}
                onClick={() => void stop(pane.id)}
              >
                {stopping.has(pane.id) ? (
                  <span className="btn-busy"><Spinner />Stopping…</span>
                ) : (
                  "Stop"
                )}
              </button>
            )}
          </div>
        );
      })}
    </div>
  );
}
