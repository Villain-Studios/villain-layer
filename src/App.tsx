import { useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api } from "./lib/api";
import { selectedTask, useStore, type View } from "./store";
import { CHAT_TASK_ID, needsYou, repoTrouble, reviewCounts, taskTotals } from "./lib/derive";
import type { TaskView } from "./lib/types";
import { Sidebar } from "./components/sidebar/Sidebar";
import { Terminals } from "./components/Terminals";
import { DiffView } from "./components/DiffView";
import { PrPanel } from "./components/PrPanel";
import { SpecView } from "./components/SpecView";
import { TicketsView } from "./components/tickets/TicketsView";
import { AgentsView } from "./components/AgentsView";
import { ChatView } from "./components/ChatView";
import { ReposView } from "./components/ReposView";
import { ReviewsView } from "./components/ReviewsView";
import { Settings } from "./components/Settings";
import { UpdateFromBase } from "./components/UpdateFromBase";
import { CupIcon, GearIcon } from "./components/icons";
import { PhoneBadge } from "./components/PhoneSettings";
import { SidebarToggle } from "./components/ui";
import { MessageCenter } from "./components/MessageCenter";
import { Watchers } from "./Watchers";
import { goTo } from "./lib/goto";

/** The view switcher, with a count on each view worth one. */
function TopBar() {
  const view = useStore((s) => s.view);
  const setView = useStore((s) => s.setView);
  const toggleSettings = useStore((s) => s.toggleSettings);
  const watchFailing = useStore((s) => s.watchFailing);
  // Counts, not the lists: a number that has not changed is not a redraw.
  // Chats have their own badge, so they must not be counted as work as well.
  const running = useStore(
    (s) => s.panes.filter((p) => p.kind === "agent" && p.running && p.task_id !== CHAT_TASK_ID).length,
  );
  const chats = useStore((s) => s.panes.filter((p) => p.task_id === CHAT_TASK_ID).length);
  // The dock icon's number, with a place in the window that says what it is.
  const waiting = useStore((s) => s.panes.filter(needsYou).length);
  const issueCount = useStore((s) => s.issues.length);
  const projectCount = useStore((s) => s.projects.length);
  const repoTroubles = useStore(
    (s) => s.projects.filter((p) => repoTrouble(p, s.repoHealth[p.id], s.tasks)).length,
  );
  // Two strings rather than one object, so an unchanged count is no redraw.
  const toReview = useStore((s) => reviewCounts(s.reviewQueue).toReview);
  const yours = useStore((s) => reviewCounts(s.reviewQueue).yours);
  // Both of the view's counts, in its tabs' order: one sum hid which side
  // was waiting, and the top bar showed only the first.
  const reviews = [toReview, yours].some((n) => n && n !== "0")
    ? {
      badge: yours === undefined ? toReview : `${toReview ?? "–"} · ${yours}`,
      title: `${toReview ?? "?"} to review · ${yours ?? "?"} opened by you`,
    }
    : {};

  const tabs: { id: View; label: string; badge?: number | string; title?: string }[] = [
    { id: "work", label: "Work", badge: running || undefined },
    { id: "tickets", label: "Tickets", badge: issueCount || undefined },
    { id: "reviews", label: "Reviews", ...reviews },
    { id: "chat", label: "Chat", badge: chats || undefined },
    { id: "repos", label: "Repos", badge: projectCount || undefined },
  ];

  return (
    // The native title bar is hidden under this one (`titleBarStyle:
    // Overlay`), so this is what the window is moved by. Tauri's attribute,
    // not CSS: `-webkit-app-region` is Chromium's, and macOS's WebKit
    // ignores it, which left a window nothing could grab. "deep" makes the
    // bar's empty parts drag; its buttons stay buttons.
    <div className="topbar" data-tauri-drag-region="deep">
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
            {t.badge !== undefined && <span className="badge" title={t.title}>{t.badge}</span>}
            {t.id === "repos" && repoTroubles > 0 && (
              <span className="badge warn" title="Repositories with a problem the Repos view can fix">
                {repoTroubles} to fix
              </span>
            )}
            {t.id === "work" && waiting > 0 && (
              <span
                className="badge warn"
                title={`${waiting} agent${waiting === 1 ? " needs" : "s need"} you — asking for something, or finished and not looked at yet`}
              >
                {waiting} need{waiting === 1 ? "s" : ""} you
              </span>
            )}
          </button>
        ))}
      </div>
      <div className="topbar-right">
        {watchFailing && (
          <span className="watch-chip" title="Task and pane polls are failing">
            couldn&apos;t refresh
          </span>
        )}
        <PhoneBadge />
        <KeepAwake />
        <MessageCenter />
        <button className="icon-btn" title="Settings" onClick={() => toggleSettings(true)}>
          <GearIcon />
        </button>
      </div>
    </div>
  );
}

/**
 * Keep the Mac awake while an agent works (STATE-7), or while any agent runs
 * when a phone's way in is open (PHONE-8). The switch is all this does:
 * `attention.rs` holds the Mac awake, so it holds while the window is away,
 * when this webview's timers may not run at all.
 */
function KeepAwake() {
  const on = useStore((s) => s.settings?.ui.keep_awake ?? false);
  const phone = useStore((s) => s.settings?.phone_open ?? false);
  const working = useStore(
    (s) => s.panes.filter((p) => p.kind === "agent" && p.running && (phone || p.activity === "working")).length,
  );
  const refreshSettings = useStore((s) => s.refreshSettings);
  const fail = useStore((s) => s.fail);

  async function toggle() {
    // The prefs as saved now, so this cannot put back an older value.
    const ui = useStore.getState().settings?.ui;
    if (!ui) return;
    try {
      await api.setUiPrefs({ ...ui, keep_awake: !ui.keep_awake });
      await refreshSettings();
    } catch (e) {
      fail(e);
    }
  }

  const title = !on
    ? "Keep the Mac awake while an agent is working"
    : working
      ? phone
        ? `Keeping the Mac awake for the phone: ${working} agent${working === 1 ? " is" : "s are"} running. Click to let it sleep.`
        : `Keeping the Mac awake: ${working} agent${working === 1 ? " is" : "s are"} working. Click to let it sleep.`
      : phone
        ? "The Mac stays awake whenever an agent is running, for the phone. None is now, so it may sleep. Click to turn off."
        : "The Mac stays awake whenever an agent is working. None is now, so it may sleep. Click to turn off.";
  return (
    <button
      className={`icon-btn awake${on ? " on" : ""}${on && working ? " holding" : ""}`}
      title={title}
      aria-pressed={on}
      onClick={() => void toggle()}
    >
      <CupIcon />
    </button>
  );
}

/** The selected task: its header, its tabs, and whichever tab is open. */
function TaskMain({ task }: { task: TaskView }) {
  const tab = useStore((s) => s.tab);
  const setTab = useStore((s) => s.setTab);
  const cursorIde = useStore((s) => s.cursorIde);
  const fail = useStore((s) => s.fail);
  // Where the board has it, from your ticket list: in step with the work or not.
  const ticket = useStore((s) => s.issues.find((i) => i.key === task.issue_key));
  // The Terminals badge counts what is open now. The task's own count comes
  // from the task poll, which can be a minute behind a pane just opened.
  const paneCount = useStore((s) => s.panes.filter((p) => p.task_id === task.id).length);
  const [updating, setUpdating] = useState(false);
  // The Spec tab's badge counts its criteria, so the spec is read with the task.
  const criteria = useStore((s) => s.specs[task.id]?.saved?.criteria.length ?? 0);
  const loadSpec = useStore((s) => s.loadSpec);
  useEffect(() => { void loadSpec(task.id).catch(() => {}); }, [task.id, loadSpec]);

  const totals = taskTotals(task);
  // What the Diff tab lists by default: files with uncommitted changes. The
  // sidebar's counts split the same work into staged, unstaged and untracked,
  // and adding those up put a 2 beside a list of forty files.
  const changed = totals.changed;

  return (
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
              title={c.broken ?? c.path}
              style={!c.exists || c.broken ? { color: "var(--red)" } : undefined}
            >
              {c.project_name}
            </span>
          ))}
        </div>
        <div className="spacer" />
        <div className="chips">
          {totals.unlinked > 0 && (
            <span
              className="chip del"
              title={task.checkouts.filter((c) => c.broken).map((c) => `${c.project_name}: ${c.broken}`).join("\n")}
            >
              {totals.unlinked} worktree{totals.unlinked === 1 ? "" : "s"} unlinked
            </span>
          )}
          {totals.missing > 0 && (
            <span className="chip warn">
              {totals.missing} worktree{totals.missing === 1 ? "" : "s"} missing
            </span>
          )}
          {totals.ahead > 0 && <span className="chip">↑{totals.ahead}</span>}
          {totals.behind > 0 && <span className="chip warn">↓{totals.behind}</span>}
          {totals.conflicted > 0 && (
            <button
              className="chip del"
              title="Resolve with an agent, or abandon the update"
              onClick={() => setUpdating(true)}
            >
              {totals.conflicted} conflict{totals.conflicted === 1 ? "" : "s"}
            </button>
          )}
          <button
            className="btn btn-sm"
            title="Fetch each repo's base branch and merge or rebase this task's branch onto it"
            onClick={() => setUpdating(true)}
          >
            Update from base
          </button>
          {cursorIde && (
            <button
              className="btn btn-sm"
              title={`Open ${task.root} in Cursor`}
              onClick={() => void api.openInCursor(task.root).catch(fail)}
            >
              Cursor ↗
            </button>
          )}
          {task.issue_url && (
            <button
              className="btn btn-sm"
              title={ticket ? `${task.issue_key} is ${ticket.status} in Jira` : undefined}
              onClick={() => void openUrl(task.issue_url!)}
            >
              {task.issue_key}{ticket && <span className="muted"> · {ticket.status}</span>} ↗
            </button>
          )}
        </div>
      </div>

      <div className="tabs">
        <button className={tab === "spec" ? "active" : ""} onClick={() => setTab("spec")}>
          Spec{criteria > 0 && <span className="badge">{criteria}</span>}
        </button>
        <button
          className={tab === "terminals" ? "active" : ""}
          onClick={() => setTab("terminals")}
        >
          Terminals<span className="badge">{paneCount}</span>
        </button>
        <button className={tab === "diff" ? "active" : ""} onClick={() => setTab("diff")}>
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
        {tab === "spec" && <SpecView task={task} />}
        {tab === "diff" && <DiffView task={task} />}
        {tab === "pr" && <PrPanel task={task} onUpdateFromBase={() => setUpdating(true)} />}
      </div>
      {updating && <UpdateFromBase task={task} onClose={() => setUpdating(false)} />}
    </>
  );
}

function Toasts() {
  const toasts = useStore((s) => s.toasts);
  const dismissToast = useStore((s) => s.dismissToast);
  return (
    <div className="toasts">
      {toasts.map((t) => (
        <div
          key={t.id}
          className={`toast ${t.kind}${t.target ? " linked" : ""}`}
          title={t.target ? "Open" : undefined}
          onClick={() => {
            if (t.target) goTo(t.target);
            dismissToast(t.id);
          }}
        >
          {t.text}
        </div>
      ))}
    </div>
  );
}

export default function App() {
  const task = useStore(selectedTask);
  const view = useStore((s) => s.view);
  const settingsOpen = useStore((s) => s.settingsOpen);
  const sidebarHidden = useStore((s) => s.sidebarHidden);
  const scale = useStore((s) => s.settings?.ui.scale ?? 1);

  return (
    <div className="shell" style={{ ["--ui-scale" as string]: scale }}>
      <Watchers />
      <TopBar />

      <div className="views">
        {view === "work" && (
          <div className={`app${sidebarHidden ? " no-sidebar" : ""}`}>
            <Sidebar />
            <div className="main">
              {task ? (
                <TaskMain key={task.id} task={task} />
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
        {view === "reviews" && <ReviewsView />}
        {view === "chat" && <ChatView />}
        {view === "repos" && <ReposView />}
      </div>

      {settingsOpen && <Settings />}
      <Toasts />
    </div>
  );
}
