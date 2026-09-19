import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api } from "./lib/api";
import { CHAT_TASK_ID, selectedTask, taskTotals, useStore, type View } from "./store";
import { Sidebar } from "./components/Sidebar";
import { Terminals } from "./components/Terminals";
import { DiffView } from "./components/DiffView";
import { PrPanel } from "./components/PrPanel";
import { TicketsView } from "./components/TicketsView";
import { AgentsView } from "./components/AgentsView";
import { ChatView } from "./components/ChatView";
import { ReposView } from "./components/ReposView";
import { Settings } from "./components/Settings";
import { GearIcon, SidebarToggle } from "./components/ui";

export default function App() {
  const task = useStore(selectedTask);
  const { view, tab, settingsOpen, toasts, panes, issues, settings, projects } = useStore();
  const setView = useStore((s) => s.setView);
  const setTab = useStore((s) => s.setTab);
  const sidebarHidden = useStore((s) => s.sidebarHidden);
  const toggleSettings = useStore((s) => s.toggleSettings);
  const refreshAll = useStore((s) => s.refreshAll);
  const refreshPanes = useStore((s) => s.refreshPanes);
  const refreshTasks = useStore((s) => s.refreshTasks);
  const dismissToast = useStore((s) => s.dismissToast);
  const fail = useStore((s) => s.fail);

  useEffect(() => { void refreshAll().catch(fail); }, [refreshAll, fail]);

  // Worktree status is cheap to poll and keeps the sidebar honest.
  useEffect(() => {
    const t = setInterval(() => void refreshTasks().catch(() => {}), 8000);
    return () => clearInterval(t);
  }, [refreshTasks]);

  // Pane activity drives the idle indicators everywhere, so keep it fresh.
  useEffect(() => {
    const t = setInterval(() => void refreshPanes().catch(() => {}), 3000);
    return () => clearInterval(t);
  }, [refreshPanes]);

  // An agent exiting is the moment worth telling someone about.
  useEffect(() => {
    const p = listen<{ pane_id: string; code: number | null }>("pty:exit", async (e) => {
      await refreshPanes().catch(() => {});
      await refreshTasks().catch(() => {});

      // Read the pane *after* refreshing: an agent that dies on start-up can
      // exit before the spawn's own refresh lands, and looking at the stale
      // list would silently drop the notification for exactly the failure the
      // user most needs to hear about.
      const state = useStore.getState();
      const pane = state.panes.find((x) => x.id === e.payload.pane_id);
      if (!pane || pane.kind !== "agent" || pane.task_id === CHAT_TASK_ID) return;

      const owner = state.tasks.find((t) => t.id === pane.task_id);
      state.toast(
        pane.exit_code === 0 ? "info" : "error",
        pane.exit_code === 0
          ? `${pane.title} finished in ${owner?.name ?? "a task"}`
          : `${pane.title} exited with ${pane.exit_code} in ${owner?.name ?? "a task"}`,
      );

      if (state.settings?.slack_connected) {
        // The backend decides whether this kind of message is muted.
        await api.slackNotify(
          `${pane.title} finished — *${owner?.name ?? pane.task_id}*`,
          owner
            ? `\`${owner.branch}\`${owner.issue_key ? ` · ${owner.issue_key}` : ""} · ${
                owner.checkouts.length
              } repo${owner.checkouts.length === 1 ? "" : "s"}`
            : undefined,
          "agent_done",
        ).catch(() => {});
      }
    });
    return () => { void p.then((un) => un()); };
  }, [refreshPanes, refreshTasks]);

  // Hitting a usage limit is the one thing worth interrupting for: the agent
  // has stopped working and will not say so again.
  useEffect(() => {
    const p = listen<{ pane_id: string; notice: string | null }>("pty:notice", async (e) => {
      await refreshPanes().catch(() => {});
      const state = useStore.getState();
      const pane = state.panes.find((x) => x.id === e.payload.pane_id);
      if (!pane) return;
      const owner = state.tasks.find((t) => t.id === pane.task_id);

      if (e.payload.notice === "trust_prompt") {
        state.toast(
          "info",
          `${pane.title} is asking whether to trust ${owner?.name ?? "the worktree"} — answer it in Terminals or it will not start.`,
        );
      } else if (e.payload.notice === "usage_limit") {
        state.toast(
          "error",
          `${pane.title} hit a usage limit in ${owner?.name ?? "a task"} — open it to hand off to another agent.`,
        );
      }
    });
    return () => { void p.then((un) => un()); };
  }, [refreshPanes]);

  const totals = task ? taskTotals(task) : null;
  const changed = totals ? totals.dirty + totals.staged : 0;
  const running = panes.filter((p) => p.kind === "agent" && p.running).length;

  const tabs: { id: View; label: string; badge?: number }[] = [
    { id: "work", label: "Work", badge: running || undefined },
    { id: "tickets", label: "Tickets", badge: issues.length || undefined },
    { id: "chat", label: "Chat" },
    { id: "repos", label: "Repos", badge: projects.length || undefined },
  ];

  return (
    <div className="shell" style={{ ["--ui-scale" as string]: settings?.ui.scale ?? 1 }}>
      <div className="topbar">
        <div className="topbar-left">
          <div className="brand">villain<span>·</span>layer</div>
        </div>
        <div className="topbar-center">
          {tabs.map((t) => (
            <button
              key={t.id}
              className={`viewtab${view === t.id ? " active" : ""}`}
              onClick={() => setView(t.id)}
            >
              {t.label}
              {t.badge !== undefined && <span className="badge">{t.badge}</span>}
            </button>
          ))}
        </div>
        <div className="topbar-right">
          <button className="icon-btn" title="Settings" onClick={() => toggleSettings(true)}>
            <GearIcon />
          </button>
        </div>
      </div>

      <div className="views">
        {view === "work" && (
          <div className={`app${sidebarHidden ? " no-sidebar" : ""}`}>
            <Sidebar />
            <div className="main">
              {task ? (
                <>
                  <div className="ws-header">
                    <SidebarToggle />
                    <h1>{task.name}</h1>
                    <span className="branch">{task.branch}</span>
                    <div className="chips">
                      {task.checkouts.map((c) => (
                        <span
                          key={c.id}
                          className="chip"
                          title={c.path}
                          style={!c.exists ? { color: "var(--red)" } : undefined}
                        >
                          {c.project_name}
                        </span>
                      ))}
                    </div>
                    <div className="spacer" />
                    <div className="chips">
                      {totals && totals.missing > 0 && (
                        <span className="chip warn">{totals.missing} worktree(s) missing</span>
                      )}
                      {totals && totals.ahead > 0 && <span className="chip">↑{totals.ahead}</span>}
                      {totals && totals.behind > 0 && (
                        <span className="chip warn">↓{totals.behind}</span>
                      )}
                      {totals && totals.conflicted > 0 && (
                        <span className="chip del">{totals.conflicted} conflicts</span>
                      )}
                      {task.issue_url && (
                        <button
                          className="btn btn-sm"
                          onClick={() => void openUrl(task.issue_url!)}
                        >
                          {task.issue_key} ↗
                        </button>
                      )}
                    </div>
                  </div>

                  <div className="tabs">
                    <button
                      className={tab === "terminals" ? "active" : ""}
                      onClick={() => setTab("terminals")}
                    >
                      Terminals<span className="badge">{task.pane_count}</span>
                    </button>
                    <button
                      className={tab === "diff" ? "active" : ""}
                      onClick={() => setTab("diff")}
                    >
                      Diff{changed > 0 && <span className="badge">{changed}</span>}
                    </button>
                    <button className={tab === "pr" ? "active" : ""} onClick={() => setTab("pr")}>
                      Pull requests
                      {task.checkouts.length > 1 && (
                        <span className="badge">{task.checkouts.length}</span>
                      )}
                    </button>
                  </div>

                  <div className="content">
                    {/* Terminals stay mounted so xterm state survives tab switches. */}
                    <div
                      style={{
                        display: tab === "terminals" ? "flex" : "none",
                        flexDirection: "column",
                        flex: 1,
                        overflow: "hidden",
                      }}
                    >
                      <Terminals task={task} />
                    </div>
                    {tab === "diff" && <DiffView task={task} />}
                    {tab === "pr" && <PrPanel task={task} />}
                  </div>
                </>
              ) : (
                // With nothing selected, show what every agent is doing rather
                // than an empty panel — the sidebar covers the task list, this
                // covers the fleet.
                <AgentsView />
              )}
            </div>
          </div>
        )}

        {view === "tickets" && <TicketsView />}
        {view === "chat" && <ChatView />}
        {view === "repos" && <ReposView />}
      </div>

      {settingsOpen && <Settings />}

      <div className="toasts">
        {toasts.map((t) => (
          <div key={t.id} className={`toast ${t.kind}`} onClick={() => dismissToast(t.id)}>
            {t.text}
          </div>
        ))}
      </div>
    </div>
  );
}
