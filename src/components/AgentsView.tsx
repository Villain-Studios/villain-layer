import { api } from "../lib/api";
import { CHAT_TASK_ID, paneState, useStore } from "../store";
import { SidebarToggle } from "./ui";

function ago(iso: string): string {
  const s = Math.max(0, Math.round((Date.now() - new Date(iso).getTime()) / 1000));
  if (s < 5) return "just now";
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  if (s < 86400) return `${Math.round(s / 3600)}h ago`;
  return `${Math.round(s / 86400)}d ago`;
}

export function AgentsView() {
  const panes = useStore((s) => s.panes);
  const tasks = useStore((s) => s.tasks);
  const agents = useStore((s) => s.agents);
  const refreshPanes = useStore((s) => s.refreshPanes);
  const select = useStore((s) => s.select);
  const setView = useStore((s) => s.setView);
  const fail = useStore((s) => s.fail);

  const working = panes.filter((p) => p.kind === "agent");
  const live = working.filter((p) => p.running);

  return (
    <div className="wide">
      <div className="wide-head">
        <SidebarToggle />
        <h2>Agents</h2>
        <span className="sub">
          {live.length} running · {working.length - live.length} finished — pick a task
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
                style={pane.notice ? { color: "var(--amber)" } : undefined}
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

            <span className="when">{ago(pane.last_output_at)}</span>

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
                onClick={() => void api.killPane(pane.id).then(() => refreshPanes()).catch(fail)}
              >
                Stop
              </button>
            )}
          </div>
        );
      })}
    </div>
  );
}
