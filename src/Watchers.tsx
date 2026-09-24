import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api } from "./lib/api";
import { prepareNotifications } from "./lib/notify";
import { CHAT_TASK_ID, stoppedOnPurpose, useStore } from "./store";
import type { NotifyTarget } from "./lib/types";

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
export function Watchers() {
  const prs = useStore((s) => s.prs);
  const tasks = useStore((s) => s.tasks);
  const view = useStore((s) => s.view);
  const githubConnected = useStore((s) => s.settings?.github_connected);
  const refreshAll = useStore((s) => s.refreshAll);
  const refreshRepoHealth = useStore((s) => s.refreshRepoHealth);
  const refreshPanes = useStore((s) => s.refreshPanes);
  const refreshPrs = useStore((s) => s.refreshPrs);
  const refreshReviewQueue = useStore((s) => s.refreshReviewQueue);
  const notifyOn = useStore((s) => s.settings?.ui.system_notifications);
  const refreshTasks = useStore((s) => s.refreshTasks);
  const setAppActive = useStore((s) => s.setAppActive);
  const toast = useStore((s) => s.toast);
  const fail = useStore((s) => s.fail);

  /**
   * When each GitHub sweep last ran. Their timers were set afresh on every
   * re-arm (each view change, return to the window, and move in or out of
   * quiet), and so were reset before ninety seconds ever passed: a PR said
   * "conflicts with dev" long after a rebase and push had fixed it. Timed
   * from here, re-arming can bring a sweep forward but never put it off.
   */
  const sweptAt = useRef({ prs: 0, reviews: 0 });

  /** The last PR state each repo was seen in, so only changes are announced. */
  const seenPrs = useRef(
    new Map<string, { verdict: string; comments: number; merged: boolean }>(),
  );

  useEffect(() => { void refreshAll().catch(fail); }, [refreshAll, fail]);
  // Once at launch too, so a repo in trouble shows on the Repos tab before
  // anyone opens it.
  useEffect(() => { void refreshRepoHealth().catch(fail); }, [refreshRepoHealth, fail]);

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
      // Timeouts and intervals share one list of timers, so either clears.
      if (prsT !== undefined) clearInterval(prsT);
      if (reviewsT !== undefined) clearInterval(reviewsT);
      tasksT = panesT = prsT = reviewsT = undefined;
    };

    /** Run `sweep` every `every` ms, the first time when it is next due. */
    const due = (key: "prs" | "reviews", every: number, sweep: () => void) => {
      const run = () => {
        sweptAt.current[key] = Date.now();
        sweep();
      };
      const wait = Math.max(0, sweptAt.current[key] + every - Date.now());
      const id: ReturnType<typeof setInterval> = setTimeout(() => {
        run();
        // guard: allow poll — GitHub cannot push to a desktop app; 90s stays inside its search limit.
        const again = setInterval(run, every);
        if (key === "prs") prsT = again;
        else reviewsT = again;
      }, wait);
      return id;
    };

    const arm = () => {
      clear();
      const onWork = useStore.getState().view === "work";
      // Off the Work view the sidebar is not on screen — there is no reason to
      // git-status every few seconds. Quiet stretches stretch further still.
      const tasksMs = !onWork ? 60_000 : quiet ? 30_000 : 12_000;
      const panesMs = quiet ? 15_000 : 5_000;
      // guard: allow poll — worktrees change under agents, editors and terminals, and git says nothing.
      tasksT = setInterval(() => void refreshTasks({ poll: true }).catch(() => {}), tasksMs);
      // guard: allow poll — a backstop: restored panes and output-timed states arrive without an event.
      panesT = setInterval(() => void refreshPanes({ poll: true }).catch(() => {}), panesMs);
      if (useStore.getState().settings?.github_connected) {
        const every = quiet ? 180_000 : 90_000;
        prsT = due("prs", every, () => void refreshPrs());
        reviewsT = due("reviews", every, () => void refreshReviewQueue({ quiet: true }));
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
        }
        // Back in front: what GitHub says is refreshed at once if it is
        // overdue, pull requests included (they were left to the timer).
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

    // guard: allow poll — measures time since the last input; "nothing happened" has no event.
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
  useEffect(() => {
    sweptAt.current.prs = Date.now();
    void refreshPrs();
  }, [refreshPrs, githubConnected]);
  useEffect(() => {
    sweptAt.current.reviews = Date.now();
    void refreshReviewQueue({ quiet: true });
  }, [refreshReviewQueue, githubConnected]);

  // Your tickets, every three minutes while the window is in front. While it
  // is away the backend looks for itself, for the banners: a hidden
  // webview's timers are the ones macOS throttles or stops.
  useEffect(() => {
    // guard: allow poll — Jira cannot push to a desktop app, and this skips itself while away.
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
    // Anything raised later, by work that outlasted the two looks above.
    const p = listen("app:notices", () => {
      void api.takeNotices().then(show).catch(() => {});
    });
    return () => {
      clearTimeout(t);
      void p.then((un) => un());
    };
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
