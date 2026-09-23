import { create } from "zustand";
import { api, errMessage } from "./lib/api";
import { read, readOneOf, write } from "./lib/persist";
import type {
  AgentStatus,
  CheckoutPr,
  JiraIssue,
  JiraIssueType,
  PaneInfo,
  Project,
  ReviewQueue,
  Settings,
  TaskView,
} from "./lib/types";

export const TABS = ["terminals", "diff", "pr"] as const;
export const VIEWS = ["work", "tickets", "chat", "repos", "reviews"] as const;
export type Tab = (typeof TABS)[number];
export type View = (typeof VIEWS)[number];

/** Panes started from the Chat view carry this instead of a real task id. */
export const CHAT_TASK_ID = "chat";

interface Toast {
  id: number;
  kind: "info" | "error" | "success";
  text: string;
}

interface State {
  projects: Project[];
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
  setAppActive: (active: boolean) => void;

  toast: (kind: Toast["kind"], text: string) => void;
  dismissToast: (id: number) => void;
  fail: (e: unknown) => void;

  refreshAll: () => Promise<void>;
  refreshRepos: () => Promise<void>;
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

/**
 * Run `work` once, sharing it with polls that overlap and queueing behind it
 * for anything that is not a poll.
 */
async function coalesce(
  slot: { get: () => Promise<void> | null; set: (p: Promise<void> | null) => void },
  poll: boolean,
  work: () => Promise<void>,
): Promise<void> {
  const current = slot.get();
  if (current) {
    if (poll) return current;
    // Wait the in-flight one out, then ask again: what it returns was asked
    // for before whatever just changed.
    await current.catch(() => {});
    if (slot.get()) return coalesce(slot, poll, work);
  }
  const run = work();
  slot.set(run);
  try {
    await run;
  } finally {
    if (slot.get() === run) slot.set(null);
  }
}

const tasksSlot = { get: () => tasksInflight, set: (p: Promise<void> | null) => { tasksInflight = p; } };
const panesSlot = { get: () => panesInflight, set: (p: Promise<void> | null) => { panesInflight = p; } };

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
      x.last_output_at !== y.last_output_at ||
      x.title !== y.title ||
      x.task_id !== y.task_id
    ) {
      return false;
    }
  }
  return true;
}

function sameTasks(a: TaskView[], b: TaskView[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    const x = a[i];
    const y = b[i];
    if (
      x.id !== y.id ||
      x.name !== y.name ||
      x.branch !== y.branch ||
      x.pane_count !== y.pane_count ||
      x.checkouts.length !== y.checkouts.length
    ) {
      return false;
    }
    for (let j = 0; j < x.checkouts.length; j++) {
      const cx = x.checkouts[j];
      const cy = y.checkouts[j];
      if (
        cx.id !== cy.id ||
        cx.changed !== cy.changed ||
        cx.exists !== cy.exists ||
        cx.status?.staged !== cy.status?.staged ||
        cx.status?.unstaged !== cy.status?.unstaged ||
        cx.status?.untracked !== cy.status?.untracked ||
        cx.status?.ahead !== cy.status?.ahead ||
        cx.status?.behind !== cy.status?.behind ||
        cx.status?.conflicted !== cy.status?.conflicted
      ) {
        return false;
      }
    }
  }
  return true;
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
  tasks: [],
  panes: [],
  prs: {},
  reviewQueue: null,
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
  // Terminals watch this themselves: one that is no longer shown detaches,
  // and the backend stops sending its redraws to a webview nobody is
  // looking at.
  setAppActive: (appActive) => {
    if (get().appActive === appActive) return;
    set({ appActive });
  },

  toast: (kind, text) => {
    const id = ++toastSeq;
    set((s) => ({ toasts: [...s.toasts, { id, kind, text }] }));
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
    try {
      const all = await api.githubAllPrs();
      // Merged, not replaced: a task the sweep could not read is left out of
      // its result, and dropping it here would blank a panel that had just
      // fetched those rows for itself.
      set((s) => ({
        prs: { ...s.prs, ...Object.fromEntries(all.map((t) => [t.task_id, t.rows])) },
      }));
    } catch {
      // Left as it was: stale rows beat empty ones.
    } finally {
      sweeping = false;
    }
  },

  setTaskPrs: (taskId, rows) => set((s) => ({ prs: { ...s.prs, [taskId]: rows } })),

  refreshSettings: async () => {
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
    if (settings.github_connected) {
      void get().refreshReviewQueue({ quiet: true });
    } else if (get().reviewQueue || get().reviewQueueError) {
      set({ reviewQueue: null, reviewQueueError: null });
    }
  },

  refreshReviewQueue: (opts) =>
    coalesce(
      { get: () => reviewsInflight, set: (p) => { reviewsInflight = p; } },
      opts?.quiet ?? false,
      async () => {
        if (!get().settings?.github_connected) return;
        // A timer tick should not flash the spinner over a list already on
        // screen. The first load has nothing to keep, so it still shows one.
        const spin = !opts?.quiet || get().reviewQueue === null;
        if (spin) set({ reviewQueueLoading: true });
        try {
          set({ reviewQueue: await api.githubReviewQueue(), reviewQueueError: null });
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
      set({ issues: page.issues, issuesTruncated: page.more, issueTypes, issuesLoaded: true });
    } catch (e) {
      if (!opts?.quiet) get().fail(e);
    } finally {
      if (spin) set({ issuesLoading: false });
    }
  },
  };
});

export const selectedTask = (s: State) =>
  s.tasks.find((t) => t.id === s.selectedTask) ?? null;

/** A running agent that has printed nothing for a while is usually waiting. */
const IDLE_AFTER_MS = 45_000;

export function paneState(pane: PaneInfo): { label: string; dot: string } {
  if (pane.running && pane.notice === "usage_limit") {
    return { label: "out of budget — hand off", dot: "gone" };
  }
  if (pane.running && pane.notice === "trust_prompt") {
    return { label: "waiting: trust this folder?", dot: "idle" };
  }
  if (!pane.running) {
    return {
      label: pane.exit_code === 0 ? "finished" : `exited ${pane.exit_code ?? "?"}`,
      dot: "gone",
    };
  }
  if (Date.now() - new Date(pane.last_output_at).getTime() > IDLE_AFTER_MS) {
    return { label: "idle — may need you", dot: "idle" };
  }
  return { label: "working", dot: "live" };
}

/**
 * Issues grouped under their epic, with the epics that have work in flight
 * first.
 *
 * "In flight" is either signal that something is actually happening: the ticket
 * is in progress in Jira, or this app already has a task open for it. Sorting
 * on it puts what you are in the middle of at the top, where an alphabetical
 * list would bury it under whatever happens to start with an A.
 */
export function groupByEpic(issues: JiraIssue[], started: Set<string> = new Set()) {
  // An epic that heads a group is represented by that header. Listing it again
  // as a card in the orphan bucket would show the same ticket twice.
  const heads = new Set(
    issues.map((i) => i.epic_key).filter((k): k is string => !!k),
  );

  const buckets = new Map<
    string,
    { key: string; summary: string; issue?: JiraIssue; issues: JiraIssue[] }
  >();

  for (const issue of issues) {
    const key = issue.epic_key ?? "";
    const bucket = buckets.get(key) ?? {
      key,
      summary: issue.epic_summary ?? "",
      issues: [],
    };
    if (!bucket.summary && issue.epic_summary) bucket.summary = issue.epic_summary;
    if (!(key === "" && heads.has(issue.key))) bucket.issues.push(issue);
    buckets.set(key, bucket);
  }

  // An epic that is itself assigned to you supplies its own title and, from the
  // header, a way to open it.
  for (const issue of issues) {
    const own = buckets.get(issue.key);
    if (own) {
      own.issue = issue;
      if (!own.summary) own.summary = issue.summary;
    }
  }

  const live = (b: { issues: JiraIssue[] }) =>
    b.issues.filter(
      (i) => i.status_category === "indeterminate" || started.has(i.key),
    ).length;

  return [...buckets.values()]
    .filter((b) => b.issues.length > 0)
    .map((b) => ({ ...b, live: live(b) }))
    .sort((a, b) => {
      // Whatever has no epic stays at the bottom either way: it is a leftovers
      // bucket, not a thing being worked on.
      if (a.key === "") return 1;
      if (b.key === "") return -1;
      if (a.live !== b.live) return b.live - a.live;
      return a.key.localeCompare(b.key);
    });
}

/** Repos bucketed by group, groups alphabetical, ungrouped last. */
export function groupProjects(projects: Project[]) {
  const buckets = new Map<string, Project[]>();
  for (const p of projects) {
    const key = p.group ?? "";
    buckets.set(key, [...(buckets.get(key) ?? []), p]);
  }
  return [...buckets.entries()]
    .sort(([a], [b]) => (a === "" ? 1 : b === "" ? -1 : a.localeCompare(b)))
    .map(([group, items]) => ({
      group,
      label: group || "ungrouped",
      projects: [...items].sort((a, b) => a.name.localeCompare(b.name)),
    }));
}

/** Rolled-up worktree state across every repo in a task. */
export function taskTotals(task: TaskView) {
  return task.checkouts.reduce(
    (acc, c) => ({
      dirty: acc.dirty + (c.status ? c.status.unstaged + c.status.untracked : 0),
      staged: acc.staged + (c.status?.staged ?? 0),
      ahead: acc.ahead + (c.status?.ahead ?? 0),
      behind: acc.behind + (c.status?.behind ?? 0),
      conflicted: acc.conflicted + (c.status?.conflicted ?? 0),
      missing: acc.missing + (c.exists ? 0 : 1),
      changed: acc.changed + c.changed,
    }),
    { dirty: 0, staged: 0, ahead: 0, behind: 0, conflicted: 0, missing: 0, changed: 0 },
  );
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

/** What a task's pull requests add up to. */
export type TaskReview =
  | "none"
  | "incomplete"
  | "open"
  | "commented"
  | "changes_requested"
  | "approved"
  | "merged";

/**
 * Roll a task's PR rows up into one answer, where every PR has to agree.
 *
 * A ticket is the unit of work even when it spans repositories, so it is not
 * approved until all of its PRs are, and not merged until all of them are.
 * One outstanding "changes requested" speaks for the whole task, because that
 * is the repo that still needs an answer before any of it lands.
 *
 * `incomplete` is the honest word for a task whose other repos have changes
 * but no PR yet: calling that "in review" would claim review of code nobody
 * has been shown.
 */
export function taskReview(rows: CheckoutPr[]): TaskReview {
  // A PR closed without merging is neither review in progress nor work that
  // landed — it is one somebody gave up on. The row keeps it so the panel can
  // say what became of it, but it answers nothing about where the task stands.
  const live = rows.filter((r) => r.pr && (r.pr.state === "open" || r.pr.merged));
  if (live.length === 0) return "none";

  // Merged is the last word: a "changes requested" left outstanding on a PR
  // that landed anyway is history, not something still to answer.
  if (live.every((r) => r.pr!.merged)) return "merged";
  if (live.some((r) => r.verdict === "changes_requested")) return "changes_requested";
  if (rows.some((r) => (!r.pr || r.pr.state !== "open") && r.changed > 0)) return "incomplete";
  if (live.every((r) => r.verdict === "approved")) return "approved";
  if (live.some((r) => r.verdict === "commented")) return "commented";
  return "open";
}

/** How many comments a task's PRs are carrying, conversation and inline both. */
export function reviewComments(rows: CheckoutPr[]): number {
  return rows.reduce(
    (n, r) =>
      n + (r.pr && r.pr.state === "open" ? r.pr.comments + r.pr.review_comments : 0),
    0,
  );
}
