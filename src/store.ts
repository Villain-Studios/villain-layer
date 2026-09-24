import { useEffect, useState } from "react";
import { create } from "zustand";
import { api, errMessage } from "./lib/api";
import { read, readOneOf, write } from "./lib/persist";
import type {
  AgentStatus,
  CheckoutPr,
  JiraIssue,
  JiraIssueType,
  Message,
  MessageKind,
  PaneInfo,
  Project,
  RepoHealth,
  ReviewQueue,
  Settings,
  Target,
  TaskView,
  TicketMove,
} from "./lib/types";

export const TABS = ["terminals", "diff", "pr"] as const;
export const VIEWS = ["work", "tickets", "chat", "repos", "reviews"] as const;
export type Tab = (typeof TABS)[number];
export type View = (typeof VIEWS)[number];

interface Toast {
  id: number;
  kind: "info" | "error" | "success";
  text: string;
  /** Where a click on it goes (NOTE-4); without one a click only dismisses. */
  target?: Target;
}

interface ToastOpts {
  target?: Target;
  /**
   * Keep it in the message center as this kind (MSG-1). Errors are kept
   * unless this says otherwise; anything else only when it says so. False
   * for news the backend has recorded already (MSG-3).
   */
  record?: MessageKind | false;
}

interface State {
  projects: Project[];
  /**
   * How each repository is doing, by project id (REPO-5). Read at launch,
   * when the Repos view opens, and after anything there changes it.
   */
  repoHealth: Record<string, RepoHealth>;
  tasks: TaskView[];
  panes: PaneInfo[];
  agents: AgentStatus[];
  /** PR rows per task id, refreshed by the background watch. */
  prs: Record<string, CheckoutPr[]>;
  /** Pull requests waiting on you, and on the configured review team. */
  reviewQueue: ReviewQueue | null;
  reviewQueueLoading: boolean;
  reviewQueueError: string | null;
  settings: Settings | null;
  issues: JiraIssue[];
  issueTypes: JiraIssueType[];
  issuesLoading: boolean;
  /** A fetch has completed, so the list — even an empty one — is a real snapshot. */
  issuesLoaded: boolean;
  /** Jira had more than the app asked for, so the list below is not all of it. */
  issuesTruncated: boolean;

  selectedTask: string | null;
  view: View;
  tab: Tab;
  sidebarHidden: boolean;
  settingsOpen: boolean;
  /** Ticket key to scroll to and highlight in the Tickets view, then clear. */
  focusIssueKey: string | null;
  /** Pane to select once its task or chat is on screen, then clear (NOTE-4). */
  focusPane: string | null;
  /** Review request (`owner/repo#n`) to scroll to and highlight, then clear. */
  focusReview: string | null;
  toasts: Toast[];
  /** Background task/pane polls have failed repeatedly — not a toast every few seconds. */
  watchFailing: boolean;
  /** Cursor IDE (the editor app / `cursor` CLI), not the cursor-agent agent. */
  cursorIde: boolean;
  /**
   * False while the window is unfocused or minimized. Polls pause and hidden
   * terminals stop painting; coming back flips this and catches up once.
   */
  appActive: boolean;

  select: (id: string | null) => void;
  setView: (v: View) => void;
  setTab: (t: Tab) => void;
  toggleSidebar: () => void;
  toggleSettings: (open?: boolean) => void;
  /** Jump to Tickets and highlight this key — for a task that already has one. */
  showIssue: (key: string) => void;
  clearFocusIssue: () => void;
  showPane: (paneId: string) => void;
  clearFocusPane: () => void;
  /** Jump to Reviews and highlight this pull request. */
  showReview: (id: string) => void;
  clearFocusReview: () => void;
  setAppActive: (active: boolean) => void;

  toast: (kind: Toast["kind"], text: string, opts?: ToastOpts) => void;
  dismissToast: (id: number) => void;
  fail: (e: unknown) => void;

  refreshAll: () => Promise<void>;
  refreshRepos: () => Promise<void>;
  refreshRepoHealth: () => Promise<void>;
  /**
   * `poll` marks a timer tick: one that lands while another is still out
   * reuses its answer. An explicit refresh — after a delete, a commit, a
   * spawn — never does: it is asking about the world after something
   * changed, and the call already in flight was asked before it did.
   */
  refreshTasks: (opts?: { poll?: boolean }) => Promise<void>;
  refreshPanes: (opts?: { poll?: boolean }) => Promise<void>;
  refreshPrs: () => Promise<void>;
  /** `quiet` is a timer tick: no toast, and no spinner if a queue is already shown. */
  refreshReviewQueue: (opts?: { quiet?: boolean }) => Promise<void>;
  /** The message center's log (MSG-1), newest first. */
  messages: Message[];
  refreshMessages: () => Promise<void>;
  /** Replace one task's PR rows, for a panel that fetched them itself. */
  setTaskPrs: (taskId: string, rows: CheckoutPr[]) => void;
  refreshSettings: () => Promise<void>;
  /** `quiet` is a timer tick: no toast, and no spinner over a list already shown. */
  refreshIssues: (opts?: { quiet?: boolean }) => Promise<void>;
}

let toastSeq = 0;

/**
 * Whether a PR sweep is already in the air.
 *
 * A sweep walks every task serially and costs four GitHub round trips per
 * repository that has one, so on a slow link it can outlast the interval that
 * starts the next. Two at once would double the API spend and let the older
 * one land last, taking the store backwards.
 */
let sweeping = false;
let reviewsInflight: Promise<void> | null = null;

/**
 * The task and pane calls currently out, so overlapping requests can be
 * coalesced rather than stacked: wake-from-sleep often fires several polls at
 * once, and each would be a git status of every hot worktree.
 *
 * Dropping the later call outright was the first version, and it dropped the
 * refresh that mattered: a task deleted while a poll happened to be in flight
 * stayed in the sidebar until the next tick, up to a minute off the Work view.
 */
let tasksInflight: Promise<void> | null = null;
let panesInflight: Promise<void> | null = null;

interface Slot {
  get: () => Promise<void> | null;
  set: (p: Promise<void> | null) => void;
  /** The one refresh queued behind the call in flight, shared by everyone who asked meanwhile. */
  next?: Promise<void> | null;
}

/**
 * Run `work` once, sharing it with polls that overlap and queueing behind it
 * for anything that is not a poll.
 *
 * Everything that queues while a call is out shares one refresh after it:
 * each hook post announces itself, and five of them during one call became
 * five calls in a row, each waiting for the last.
 */
async function coalesce(slot: Slot, poll: boolean, work: () => Promise<void>): Promise<void> {
  const current = slot.get();
  if (current) {
    if (poll) return current;
    // Wait the in-flight one out, then ask again: what it returns was asked
    // for before whatever just changed.
    if (!slot.next) {
      slot.next = current
        .catch(() => {})
        .then(() => {
          slot.next = null;
          return coalesce(slot, false, work);
        });
    }
    return slot.next;
  }
  const run = work();
  slot.set(run);
  try {
    await run;
  } finally {
    if (slot.get() === run) slot.set(null);
  }
}

const tasksSlot: Slot = { get: () => tasksInflight, set: (p) => { tasksInflight = p; } };
const panesSlot: Slot = { get: () => panesInflight, set: (p) => { panesInflight = p; } };
const reviewsSlot: Slot = { get: () => reviewsInflight, set: (p) => { reviewsInflight = p; } };
let messagesInflight: Promise<void> | null = null;
const messagesSlot: Slot = { get: () => messagesInflight, set: (p) => { messagesInflight = p; } };

function sameMessages(a: Message[], b: Message[]): boolean {
  return a.length === b.length
    && a.every((m, i) => m.id === b[i].id && m.at === b[i].at && m.read === b[i].read && m.count === b[i].count);
}

/**
 * When each task's PR rows were last fetched. A sweep walks every task and
 * can take a minute on a slow link; landing after a panel fetched its own
 * rows — the PR just opened — it put back the world from before, and the
 * panel offered to open the PR again.
 */
const prsFetchedAt = new Map<string, number>();
/** "ACME:review": a project and stage already told to choose a status, this run. */
const askedFlow = new Set<string>();

/** Say what the PR sweep did about tickets (TKT-8). */
function reportTickets(moves: TicketMove[], toast: State["toast"]) {
  for (const m of moves) {
    const target: Target = `ticket:${m.key}`;
    if (m.moved_to) toast("success", `${m.key} → ${m.moved_to}`, { target, record: "ticket" });
    else if (m.error) toast("error", m.error, { target, record: "ticket" });
    else if (m.unchosen) {
      const project = m.key.split("-")[0];
      if (askedFlow.has(`${project}:${m.stage}`)) continue;
      askedFlow.add(`${project}:${m.stage}`);
      toast(
        "info",
        m.stage === "review"
          ? `${m.key}'s pull request is ready for review. Choose which ${project} status means review in Settings → Jira, and tickets will move there.`
          : `Every pull request for ${m.key} has merged. Choose where merged ${project} tickets go in Settings → Jira.`,
      );
    }
  }
}

/** Skip a React storm when a poll returns the same world we already have. */
function samePanes(a: PaneInfo[], b: PaneInfo[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    const x = a[i];
    const y = b[i];
    if (
      x.id !== y.id ||
      x.running !== y.running ||
      x.exit_code !== y.exit_code ||
      x.notice !== y.notice ||
      x.activity !== y.activity ||
      x.activity_since !== y.activity_since ||
      x.title !== y.title ||
      x.task_id !== y.task_id
    ) {
      return false;
    }
  }
  return true;
}

/**
 * Whole, not field by field. A hand-picked list missed the base: changed in
 * the PR panel, Update from base went on saying "← origin/main" while the
 * backend merged the new one. A few dozen tasks stringify in well under a
 * millisecond, and nothing in them changes on its own between polls.
 */
function sameTasks(a: TaskView[], b: TaskView[]): boolean {
  return a.length === b.length && JSON.stringify(a) === JSON.stringify(b);
}

/**
 * Consecutive quiet failures of the task/pane polls. One blip is noise; two
 * in a row usually means the backend is unreachable or a token is gone.
 */
let watchStreak = 0;

/**
 * Drop a remembered selection whose task is gone.
 *
 * The id outlives the task in storage, and a dangling one would leave the app
 * opening on a task that no longer exists — an empty pane bar and no way back.
 */
function stillThere(tasks: TaskView[], selected: string | null): string | null {
  if (!selected || tasks.some((t) => t.id === selected)) return selected;
  write("selectedTask", null);
  return null;
}

export const useStore = create<State>((set, get) => {
  const watchOk = () => {
    watchStreak = 0;
    if (get().watchFailing) set({ watchFailing: false });
  };
  const watchFail = () => {
    watchStreak += 1;
    if (watchStreak >= 2) set({ watchFailing: true });
  };

  return {
  projects: [],
  repoHealth: {},
  tasks: [],
  panes: [],
  prs: {},
  reviewQueue: null,
  messages: [],
  reviewQueueLoading: false,
  reviewQueueError: null,
  agents: [],
  settings: null,
  issues: [],
  issueTypes: [],
  issuesLoading: false,
  issuesLoaded: false,
  issuesTruncated: false,

  // Where the app was left. Restored so reopening lands on the work in
  // progress rather than on an empty shell, alongside the panes themselves.
  selectedTask: read<string | null>("selectedTask", null),
  view: readOneOf("view", VIEWS, "work"),
  tab: readOneOf("tab", TABS, "terminals"),
  sidebarHidden: read("sidebarHidden", false),
  settingsOpen: false,
  focusIssueKey: null,
  focusPane: null,
  focusReview: null,
  toasts: [],
  watchFailing: false,
  cursorIde: false,
  appActive: true,

  // Selecting a task is always a request to look at it.
  select: (id) => set({ selectedTask: id, tab: "terminals", view: "work" }),
  setView: (view) => set({ view }),
  setTab: (tab) => set({ tab }),
  toggleSidebar: () => set((s) => ({ sidebarHidden: !s.sidebarHidden })),
  toggleSettings: (open) => set((s) => ({ settingsOpen: open ?? !s.settingsOpen })),
  showIssue: (key) => set({ view: "tickets", focusIssueKey: key }),
  clearFocusIssue: () => set({ focusIssueKey: null }),
  showPane: (focusPane) => set({ focusPane }),
  clearFocusPane: () => set({ focusPane: null }),
  showReview: (id) => set({ view: "reviews", focusReview: id }),
  clearFocusReview: () => set({ focusReview: null }),
  // Terminals watch this themselves: one that is no longer shown detaches,
  // and the backend stops sending its redraws to a webview nobody is
  // looking at.
  setAppActive: (appActive) => {
    if (get().appActive === appActive) return;
    set({ appActive });
  },

  toast: (kind, text, opts) => {
    const id = ++toastSeq;
    set((s) => ({ toasts: [...s.toasts, { id, kind, text, target: opts?.target }] }));
    const keep = opts?.record ?? (kind === "error" ? "error" : false);
    // Swallowed: failing to keep an error must not raise another.
    if (keep) void api.addMessage(keep, kind, text, opts?.target).catch(() => {});
    setTimeout(() => get().dismissToast(id), kind === "error" ? 8000 : 4000);
  },
  dismissToast: (id) => set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
  fail: (e) => get().toast("error", errMessage(e)),

  refreshAll: async () => {
    // Settings (and with them the Jira list) must not wait on a full git-status
    // of every worktree: that is the slow part of boot, and Tickets used to
    // sit black until it finished. Kick both off together.
    const settingsP = get().refreshSettings();
    const focus = get().selectedTask;
    const coreP = Promise.all([
      api.listProjects(),
      api.listTasks(focus),
      api.listPanes(),
      api.listAgents(),
      api.cursorIdeInstalled().catch(() => false),
    ]).then(([projects, tasks, panes, agents, cursorIde]) => {
      set((s) => ({
        projects, tasks, panes, agents, cursorIde,
        selectedTask: stillThere(tasks, s.selectedTask),
      }));
      watchOk();
    });
    await Promise.all([settingsP, coreP]);
  },

  refreshRepos: async () => set({ projects: await api.listProjects() }),

  refreshRepoHealth: async () => {
    const rows = await api.repoHealth();
    set({ repoHealth: Object.fromEntries(rows.map((h) => [h.project_id, h])) });
  },

  refreshTasks: (opts) =>
    coalesce(tasksSlot, opts?.poll ?? false, async () => {
      try {
        // Focus the selected task so its worktrees stay fresh; everything else
        // reuses the backend status cache unless it has a running agent.
        const tasks = await api.listTasks(get().selectedTask);
        set((s) => {
          if (sameTasks(s.tasks, tasks)) return s;
          return {
            tasks,
            selectedTask: stillThere(tasks, s.selectedTask),
            // PR rows for a task that has been deleted have nothing to hang off any
            // more, and the watch would keep comparing against them for good.
            prs: Object.fromEntries(
              Object.entries(s.prs).filter(([id]) => tasks.some((t) => t.id === id)),
            ),
          };
        });
        watchOk();
      } catch (e) {
        watchFail();
        throw e;
      }
    }),
  refreshPanes: (opts) =>
    coalesce(panesSlot, opts?.poll ?? false, async () => {
      try {
        const panes = await api.listPanes();
        set((s) => (samePanes(s.panes, panes) ? s : { panes }));
        watchOk();
      } catch (e) {
        watchFail();
        throw e;
      }
    }),

  /**
   * Ask GitHub what has happened to every task's pull requests.
   *
   * Quiet on failure: this runs on a timer whether or not anyone is looking,
   * and a token that has expired or a network that has gone away should not
   * put a toast on screen every couple of minutes.
   */
  refreshPrs: async () => {
    if (sweeping || !get().settings?.github_connected) return;
    sweeping = true;
    const began = Date.now();
    try {
      const all = await api.githubAllPrs();
      // Merged, not replaced: a task the sweep could not read is left out of
      // its result, and dropping it here would blank a panel that had just
      // fetched those rows for itself. Nor over rows fetched after it began.
      const fresh = all.filter((t) => (prsFetchedAt.get(t.task_id) ?? 0) < began);
      for (const t of fresh) prsFetchedAt.set(t.task_id, began);
      set((s) => ({
        prs: { ...s.prs, ...Object.fromEntries(fresh.map((t) => [t.task_id, t.rows])) },
      }));
      const moves = all.map((t) => t.ticket).filter((m): m is TicketMove => m !== null);
      reportTickets(moves, get().toast);
      if (moves.some((m) => m.moved_to)) void get().refreshIssues({ quiet: true });
    } catch {
      // Left as it was: stale rows beat empty ones.
    } finally {
      sweeping = false;
    }
  },

  setTaskPrs: (taskId, rows) => {
    prsFetchedAt.set(taskId, Date.now());
    set((s) => ({ prs: { ...s.prs, [taskId]: rows } }));
  },

  refreshSettings: async () => {
    const before = get().settings;
    const settings = await api.getSettings();
    set({ settings });
    // Only until the first list lands: someone with nothing assigned has an
    // empty list for good, and every settings change re-fetched 500 issues.
    if (settings.jira_connected && !get().issuesLoaded && !get().issuesLoading) {
      void get().refreshIssues();
    }
    // Disconnecting has to take the list with it, or the badge keeps counting
    // tickets from a site the app can no longer reach.
    if (!settings.jira_connected && (get().issues.length > 0 || get().issuesLoaded)) {
      set({ issues: [], issueTypes: [], issuesTruncated: false, issuesLoaded: false });
    }
    // Only when GitHub itself changed. Every slider moved and every update
    // from base refreshed settings, and each ran a search — which GitHub
    // allows thirty of a minute.
    const githubChanged =
      before?.github_connected !== settings.github_connected ||
      JSON.stringify(before?.github) !== JSON.stringify(settings.github);
    if (settings.github_connected) {
      if (githubChanged || get().reviewQueue === null) void get().refreshReviewQueue({ quiet: true });
    } else if (get().reviewQueue || get().reviewQueueError) {
      set({ reviewQueue: null, reviewQueueError: null });
    }
  },

  refreshReviewQueue: (opts) =>
    coalesce(
      reviewsSlot,
      opts?.quiet ?? false,
      async () => {
        if (!get().settings?.github_connected) return;
        // A timer tick should not flash the spinner over a list already on
        // screen. The first load has nothing to keep, so it still shows one.
        const spin = !opts?.quiet || get().reviewQueue === null;
        if (spin) set({ reviewQueueLoading: true });
        try {
          const queue = await api.githubReviewQueue();
          // Disconnected while this was out: the list goes with the
          // connection, or the badge counts reviews from nothing connected.
          if (!get().settings?.github_connected) return;
          set({ reviewQueue: queue, reviewQueueError: null });
        } catch (e) {
          // A timer tick stays quiet — the view shows the message — but a
          // Refresh you pressed should also say so up top.
          set({ reviewQueueError: errMessage(e) });
          if (!opts?.quiet) get().fail(e);
        } finally {
          if (spin) set({ reviewQueueLoading: false });
        }
      },
    ),

  refreshMessages: () =>
    coalesce(messagesSlot, false, async () => {
      const messages = await api.listMessages();
      if (!sameMessages(get().messages, messages)) set({ messages });
    }),

  refreshIssues: async (opts) => {
    if (!get().settings?.jira_connected) return;
    // A timer tick should not flash the spinner over a list already on screen.
    const spin = !opts?.quiet || !get().issuesLoaded;
    if (spin) set({ issuesLoading: true });
    try {
      const [page, issueTypes] = await Promise.all([
        api.jiraIssues(),
        // Types rarely change and are cached in the backend; a failure here
        // must not stop the issues themselves from showing.
        api.jiraIssueTypes().catch(() => get().issueTypes),
      ]);
      // As for reviews: an answer from a site disconnected meanwhile is not kept.
      if (!get().settings?.jira_connected) return;
      set({ issues: page.issues, issuesTruncated: page.more, issueTypes, issuesLoaded: true });
    } catch (e) {
      if (!opts?.quiet) get().fail(e);
    } finally {
      if (spin) set({ issuesLoading: false });
    }
  },
  };
});

/**
 * Panes stopped on purpose — Stop, or the old agent in a handoff — whose exit
 * is not news.
 *
 * `kill_pane` keeps the pane on screen, so its exit arrives like any other:
 * stopping an agent put "exited with 143" up as an error and posted "finished"
 * to Slack for work that had not finished.
 */
const stopping = new Set<string>();
export function markStopping(paneId: string) {
  stopping.add(paneId);
}
/** Whether this exit was asked for. Answers once. */
export function stoppedOnPurpose(paneId: string): boolean {
  return stopping.delete(paneId);
}

export const selectedTask = (s: State) =>
  s.tasks.find((t) => t.id === s.selectedTask) ?? null;

/**
 * The time, for labels that change with it: "2m ago".
 *
 * Those were worked out from `Date.now()` whenever something happened to
 * redraw, and a pane that has gone quiet is exactly one that stops changing —
 * so nothing redrew, and the label froze. Ticks only while the window is in
 * front.
 */
export function useNow(everyMs: number): number {
  const active = useStore((s) => s.appActive);
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    // guard: allow poll — a clock for "3m ago" labels, stopped while the window is away.
    const t = setInterval(() => setNow(Date.now()), everyMs);
    return () => clearInterval(t);
  }, [active, everyMs]);
  return now;
}

// Remember where the app is, so reopening it lands back there rather than on
// an empty shell. Done by subscription rather than in each setter: the panes
// are restored whatever moved the selection, and this should be too.
let lastLayout = "";
useStore.subscribe((s) => {
  const now = JSON.stringify([s.selectedTask, s.view, s.tab, s.sidebarHidden]);
  if (now === lastLayout) return;
  lastLayout = now;
  write("selectedTask", s.selectedTask);
  write("view", s.view);
  write("tab", s.tab);
  write("sidebarHidden", s.sidebarHidden);
});
