import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api } from "./lib/api";
import { prepareNotifications } from "./lib/notify";
import { CHAT_TASK_ID, needsYou, selectedTask, stoppedOnPurpose, taskTotals, useStore, type View } from "./store";
import type { NotifyTarget, TaskView } from "./lib/types";
import { Sidebar } from "./components/Sidebar";
import { Terminals } from "./components/Terminals";
import { DiffView } from "./components/DiffView";
import { PrPanel } from "./components/PrPanel";
import { TicketsView } from "./components/TicketsView";
import { AgentsView } from "./components/AgentsView";
import { ChatView } from "./components/ChatView";
import { ReposView } from "./components/ReposView";
import { ReviewsView } from "./components/ReviewsView";
import { Settings } from "./components/Settings";
import { UpdateFromBase } from "./components/UpdateFromBase";
import { GearIcon, SidebarToggle } from "./components/ui";

/**
 * Tauri emits an unfocused event while the window is still coming up.
 * Honouring it paused polls and kicked a catch-up list_tasks that raced the
 * boot refresh — two full git-status sweeps on every launch.
 *
 * Measured from the page loading, not from the effect below: that re-runs on
 * every change of view, and restarting the grace each time meant switching
 * away within a couple of seconds of a click was ignored, with the polls left
 * running behind a window nobody was looking at.
 */
const BOOTED_AT = Date.now();
const BOOT_GRACE_MS = 2500;

/**
 * The polls, the event listeners and the announcements: everything the app
 * does on its own rather than draws.
 *
 * Separate from what it draws so that what it listens to does not redraw the
 * window. As one component, a pane poll landing while an agent printed —
 * every five seconds — re-rendered whatever view was open, the whole ticket
 * board or the whole diff included, to update a badge.
 */
function Watchers() {
  const prs = useStore((s) => s.prs);
  const tasks = useStore((s) => s.tasks);
  const view = useStore((s) => s.view);
  const githubConnected = useStore((s) => s.settings?.github_connected);
  const refreshAll = useStore((s) => s.refreshAll);
  const refreshPanes = useStore((s) => s.refreshPanes);
  const refreshPrs = useStore((s) => s.refreshPrs);
  const refreshReviewQueue = useStore((s) => s.refreshReviewQueue);
  const notifyOn = useStore((s) => s.settings?.ui.system_notifications);
  const refreshTasks = useStore((s) => s.refreshTasks);
  const setAppActive = useStore((s) => s.setAppActive);
  const toast = useStore((s) => s.toast);
  const fail = useStore((s) => s.fail);

  /** The last PR state each repo was seen in, so only changes are announced. */
  const seenPrs = useRef(
    new Map<string, { verdict: string; comments: number; merged: boolean }>(),
  );

  useEffect(() => { void refreshAll().catch(fail); }, [refreshAll, fail]);

  // Polls only while the window is in front, and slow down further when the
  // window is focused but nobody has touched it — leaving an 8s full-repo
  // sweep running forever is what made "just leave it open" feel busy.
  useEffect(() => {
    let tasksT: ReturnType<typeof setInterval> | undefined;
    let panesT: ReturnType<typeof setInterval> | undefined;
    let prsT: ReturnType<typeof setInterval> | undefined;
    let reviewsT: ReturnType<typeof setInterval> | undefined;
    let idleT: ReturnType<typeof setInterval> | undefined;
    let unFocus: (() => void) | undefined;
    let alive = true;
    let quiet = false;
    let lastInput = Date.now();
    const QUIET_AFTER_MS = 45_000;

    const clear = () => {
      if (tasksT !== undefined) clearInterval(tasksT);
      if (panesT !== undefined) clearInterval(panesT);
      if (prsT !== undefined) clearInterval(prsT);
      if (reviewsT !== undefined) clearInterval(reviewsT);
      tasksT = panesT = prsT = reviewsT = undefined;
    };

    const arm = () => {
      clear();
      const onWork = useStore.getState().view === "work";
      // Off the Work view the sidebar is not on screen — there is no reason to
      // git-status every few seconds. Quiet stretches stretch further still.
      const tasksMs = !onWork ? 60_000 : quiet ? 30_000 : 12_000;
      const panesMs = quiet ? 15_000 : 5_000;
      tasksT = setInterval(() => void refreshTasks({ poll: true }).catch(() => {}), tasksMs);
      panesT = setInterval(() => void refreshPanes({ poll: true }).catch(() => {}), panesMs);
      if (useStore.getState().settings?.github_connected) {
        prsT = setInterval(() => void refreshPrs(), quiet ? 180_000 : 90_000);
        reviewsT = setInterval(
          () => void refreshReviewQueue({ quiet: true }),
          quiet ? 180_000 : 90_000,
        );
      }
    };

    const setActive = (active: boolean) => {
      if (!alive) return;
      if (!active && Date.now() - BOOTED_AT < BOOT_GRACE_MS) return;
      const was = useStore.getState().appActive;
      setAppActive(active);
      if (active) {
        if (!was) {
          quiet = false;
          lastInput = Date.now();
          void refreshTasks({ poll: true }).catch(() => {});
          void refreshPanes({ poll: true }).catch(() => {});
          if (useStore.getState().settings?.github_connected) {
            void refreshReviewQueue({ quiet: true });
          }
        }
        arm();
      } else {
        clear();
      }
    };

    const onInput = () => {
      lastInput = Date.now();
      if (quiet && useStore.getState().appActive) {
        quiet = false;
        arm();
      }
    };

    if (useStore.getState().appActive) arm();

    void getCurrentWindow()
      .onFocusChanged(({ payload: focused }) => setActive(focused))
      .then((un) => { if (alive) unFocus = un; else un(); });

    // Minimize/restore: focus alone does not cover every path on macOS.
    const onVis = () => {
      if (document.visibilityState === "hidden") setActive(false);
      else void getCurrentWindow().isFocused().then((f) => setActive(f)).catch(() => setActive(true));
    };
    // App switch: visibilitychange often does not fire on macOS; blur/focus does.
    const onBlur = () => setActive(false);
    const onFocus = () => setActive(true);

    document.addEventListener("visibilitychange", onVis);
    window.addEventListener("blur", onBlur);
    window.addEventListener("focus", onFocus);
    window.addEventListener("pointerdown", onInput);
    window.addEventListener("keydown", onInput);

    idleT = setInterval(() => {
      if (!useStore.getState().appActive || quiet) return;
      if (Date.now() - lastInput < QUIET_AFTER_MS) return;
      quiet = true;
      arm();
    }, 5_000);

    return () => {
      alive = false;
      clear();
      if (idleT !== undefined) clearInterval(idleT);
      unFocus?.();
      document.removeEventListener("visibilitychange", onVis);
      window.removeEventListener("blur", onBlur);
      window.removeEventListener("focus", onFocus);
      window.removeEventListener("pointerdown", onInput);
      window.removeEventListener("keydown", onInput);
    };
  }, [refreshTasks, refreshPanes, refreshPrs, refreshReviewQueue, setAppActive, githubConnected, view]);

  // One sweep as soon as GitHub is available; the timer above takes it from
  // there. Kept out of the polling effect on purpose: that one re-runs on
  // every change of view, and a sweep per tab switch is four calls per open
  // pull request each time the Tickets tab is glanced at.
  useEffect(() => { void refreshPrs(); }, [refreshPrs, githubConnected]);
  useEffect(() => {
    void refreshReviewQueue({ quiet: true });
  }, [refreshReviewQueue, githubConnected]);

  // Your tickets, every three minutes while the window is in front. While it
  // is away the backend looks for itself, for the banners: a hidden
  // webview's timers are the ones macOS throttles or stops.
  useEffect(() => {
    const t = setInterval(() => {
      const s = useStore.getState();
      if (s.appActive && s.settings?.jira_connected) void s.refreshIssues({ quiet: true });
    }, 180_000);
    return () => clearInterval(t);
  }, []);

  useEffect(() => {
    if (!notifyOn) return;
    void prepareNotifications().catch(() => {});
  }, [notifyOn]);

  // An agent exiting is the moment worth telling someone about.
  useEffect(() => {
    const p = listen<{ pane_id: string; code: number | null }>("pty:exit", async (e) => {
      await refreshPanes().catch(() => {});
      // The tasks only for their counts, so a sweep already in flight will
      // do. Asked for outright, each exit queued a git status of every hot
      // worktree behind the last: deleting a task with four agents was four
      // sweeps in a row.
      await refreshTasks({ poll: true }).catch(() => {});

      // Read the pane *after* refreshing: an agent that dies on start-up can
      // exit before the spawn's own refresh lands, and looking at the stale
      // list would silently drop the notification for exactly the failure the
      // user most needs to hear about.
      const state = useStore.getState();
      const pane = state.panes.find((x) => x.id === e.payload.pane_id);
      if (!pane) return;

      // A shell you typed `exit` into has nothing left to show. Agents are
      // left alone: their last screen is the point, and one that died holds
      // the reason why.
      if (pane.kind === "shell") {
        await api.closePane(pane.id).catch(() => {});
        await refreshPanes().catch(() => {});
        return;
      }

      if (pane.kind !== "agent" || pane.task_id === CHAT_TASK_ID) return;
      if (stoppedOnPurpose(pane.id)) return;

      const owner = state.tasks.find((t) => t.id === pane.task_id);
      const how = pane.exit_code === 0 ? "finished" : `exited with ${pane.exit_code ?? "?"}`;
      state.toast(
        pane.exit_code === 0 ? "info" : "error",
        `${pane.title} ${how} in ${owner?.name ?? "a task"}`,
      );

      if (state.settings?.slack_connected) {
        // The backend decides whether this kind of message is muted. Worded
        // from the exit code: a crash reported as "finished" is worse than
        // no message.
        await api.slackNotify(
          `${pane.title} ${how} — *${owner?.name ?? pane.task_id}*`,
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

  // An agent's own hooks said what it is doing now. Its row, its dot and the
  // count on the Work tab follow straight away rather than at the next poll.
  useEffect(() => {
    const p = listen<string>("pty:activity", () => { void refreshPanes().catch(() => {}); });
    return () => { void p.then((un) => un()); };
  }, [refreshPanes]);

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
        // A chat has no handoff; a task's agent does.
        state.toast(
          "error",
          pane.task_id === CHAT_TASK_ID
            ? `${pane.title} hit a usage limit in a chat — start another chat with a different agent.`
            : `${pane.title} hit a usage limit in ${owner?.name ?? "a task"} — open it to hand off to another agent.`,
        );
      }
    });
    return () => { void p.then((un) => un()); };
  }, [refreshPanes]);

  // Startup notices the backend queued before the UI was listening (MCP bind
  // failure, restore truncation). Drained on mount and once more after restore
  // has usually finished — emitting at setup time is silent.
  useEffect(() => {
    const show = (notices: { kind: string; text: string }[]) => {
      const toast = useStore.getState().toast;
      for (const n of notices) {
        toast(n.kind === "error" ? "error" : "info", n.text);
      }
    };
    void api.takeNotices().then(show).catch(() => {});
    const t = setTimeout(() => {
      void api.takeNotices().then(show).catch(() => {});
    }, 4000);
    return () => clearTimeout(t);
  }, []);

  // Say when a review lands, once.
  //
  // Only a change against something already seen is worth a toast: the first
  // sweep of a launch records what is there without announcing it, or every
  // open PR would report its weeks-old verdict as news. A PR appearing for the
  // first time is silent too — you opened it, from here.
  useEffect(() => {
    const seen = seenPrs.current;

    for (const [taskId, rows] of Object.entries(prs)) {
      const owner = tasks.find((t) => t.id === taskId);
      const where = owner?.name ?? "a task";

      for (const row of rows) {
        if (!row.pr) continue;
        // The PR number is part of the key: a branch retried after a closed
        // attempt gets a new PR, and comparing it against the old one's
        // comment count would announce comments that were never written.
        const key = `${taskId}:${row.checkout_id}:${row.pr.number}`;
        const now = {
          verdict: row.verdict,
          comments: row.pr.comments + row.pr.review_comments,
          merged: row.pr.merged,
        };
        const was = seen.get(key);
        const pr = `${row.repo} #${row.pr.number}`;

        // Closed is final, and the sweep stops asking about its reviews once
        // it is — so it has to be settled before the "lost what was known"
        // check below, or the one closing worth a word would be the one
        // swallowed. Merged is only news when it was seen open here first.
        if (row.pr.state !== "open") {
          if (was && now.merged && !was.merged) toast("success", `${pr} merged — ${where}`);
          seen.set(key, now);
          continue;
        }

        // A reviews or detail call that failed comes back as verdict "none".
        // Losing what was known is not news, and recording it would make the
        // next sweep that succeeds announce a week-old approval as though it
        // had just landed.
        const degraded = was && now.verdict === "none" && was.verdict !== "none";
        if (!degraded) seen.set(key, now);
        if (!was || degraded) continue;

        const by = [...row.reviews]
          .reverse()
          .find((r) => r.state === "APPROVED" || r.state === "CHANGES_REQUESTED")?.author;

        if (now.verdict !== was.verdict && now.verdict === "approved") {
          toast("success", `${pr} approved${by ? ` by ${by}` : ""} — ${where}`);
        } else if (now.verdict !== was.verdict && now.verdict === "changes_requested") {
          toast("error", `${pr}: changes requested${by ? ` by ${by}` : ""} — ${where}`);
        } else if (now.comments > was.comments) {
          const n = now.comments - was.comments;
          toast("info", `${n} new comment${n === 1 ? "" : "s"} on ${pr} — ${where}`);
        }
      }
    }
  }, [prs, tasks, toast]);

  useEffect(() => {
    const p = listen<NotifyTarget>("system-notify-click", (e) => {
      const target = e.payload;
      const s = useStore.getState();
      if (target === "reviews" || target === "tickets" || target === "chat") s.setView(target);
      // Nothing selected is the overview of every agent.
      else if (target === "work") s.select(null);
      else if (target.startsWith("task:")) {
        const id = target.slice("task:".length);
        // Deleted while the banner sat there: the overview, not a blank task.
        s.select(s.tasks.some((t) => t.id === id) ? id : null);
      }
      const win = getCurrentWindow();
      void win.unminimize().catch(() => {});
      void win.show().catch(() => {});
      void win.setFocus().catch(() => {});
    });
    return () => { void p.then((un) => un()); };
  }, []);

  return null;
}

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
  const reviewCount = useStore(
    (s) => (s.reviewQueue?.mine.length ?? 0) + (s.reviewQueue?.team?.prs.length ?? 0),
  );

  const tabs: { id: View; label: string; badge?: number }[] = [
    { id: "work", label: "Work", badge: running || undefined },
    { id: "tickets", label: "Tickets", badge: issueCount || undefined },
    { id: "reviews", label: "Reviews", badge: reviewCount || undefined },
    { id: "chat", label: "Chat", badge: chats || undefined },
    { id: "repos", label: "Repos", badge: projectCount || undefined },
  ];

  return (
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
        <button className="icon-btn" title="Settings" onClick={() => toggleSettings(true)}>
          <GearIcon />
        </button>
      </div>
    </div>
  );
}

/** The selected task: its header, its tabs, and whichever tab is open. */
function TaskMain({ task }: { task: TaskView }) {
  const tab = useStore((s) => s.tab);
  const setTab = useStore((s) => s.setTab);
  const cursorIde = useStore((s) => s.cursorIde);
  const fail = useStore((s) => s.fail);
  // The Terminals badge counts what is open now. The task's own count comes
  // from the task poll, which can be a minute behind a pane just opened.
  const paneCount = useStore((s) => s.panes.filter((p) => p.task_id === task.id).length);
  const [updating, setUpdating] = useState(false);

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
              title={c.path}
              style={!c.exists ? { color: "var(--red)" } : undefined}
            >
              {c.project_name}
            </span>
          ))}
        </div>
        <div className="spacer" />
        <div className="chips">
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
            <button className="btn btn-sm" onClick={() => void openUrl(task.issue_url!)}>
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
        <div key={t.id} className={`toast ${t.kind}`} onClick={() => dismissToast(t.id)}>
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
