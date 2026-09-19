import { create } from "zustand";
import { api, errMessage } from "./lib/api";
import { read, readOneOf, write } from "./lib/persist";
import type {
  AgentStatus, JiraIssue, JiraIssueType, PaneInfo, Project, RepoRule, RepoSet, Settings,
  TaskView,
} from "./lib/types";

export const TABS = ["terminals", "diff", "pr"] as const;
export const VIEWS = ["work", "tickets", "chat", "repos"] as const;
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
  repoSets: RepoSet[];
  repoRules: RepoRule[];
  tasks: TaskView[];
  panes: PaneInfo[];
  agents: AgentStatus[];
  settings: Settings | null;
  issues: JiraIssue[];
  issueTypes: JiraIssueType[];
  issuesLoading: boolean;
  /** Jira had more than the app asked for, so the list below is not all of it. */
  issuesTruncated: boolean;

  selectedTask: string | null;
  view: View;
  tab: Tab;
  sidebarHidden: boolean;
  settingsOpen: boolean;
  toasts: Toast[];

  select: (id: string | null) => void;
  setView: (v: View) => void;
  setTab: (t: Tab) => void;
  toggleSidebar: () => void;
  toggleSettings: (open?: boolean) => void;

  toast: (kind: Toast["kind"], text: string) => void;
  dismissToast: (id: number) => void;
  fail: (e: unknown) => void;

  refreshAll: () => Promise<void>;
  refreshRepos: () => Promise<void>;
  refreshTasks: () => Promise<void>;
  refreshPanes: () => Promise<void>;
  refreshSettings: () => Promise<void>;
  refreshIssues: () => Promise<void>;
}

let toastSeq = 0;

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

export const useStore = create<State>((set, get) => ({
  projects: [],
  repoSets: [],
  repoRules: [],
  tasks: [],
  panes: [],
  agents: [],
  settings: null,
  issues: [],
  issueTypes: [],
  issuesLoading: false,
  issuesTruncated: false,

  // Where the app was left. Restored so reopening lands on the work in
  // progress rather than on an empty shell, alongside the panes themselves.
  selectedTask: read<string | null>("selectedTask", null),
  view: readOneOf("view", VIEWS, "work"),
  tab: readOneOf("tab", TABS, "terminals"),
  sidebarHidden: read("sidebarHidden", false),
  settingsOpen: false,
  toasts: [],

  // Selecting a task is always a request to look at it.
  select: (id) => set({ selectedTask: id, tab: "terminals", view: "work" }),
  setView: (view) => set({ view }),
  setTab: (tab) => set({ tab }),
  toggleSidebar: () => set((s) => ({ sidebarHidden: !s.sidebarHidden })),
  toggleSettings: (open) => set((s) => ({ settingsOpen: open ?? !s.settingsOpen })),

  toast: (kind, text) => {
    const id = ++toastSeq;
    set((s) => ({ toasts: [...s.toasts, { id, kind, text }] }));
    setTimeout(() => get().dismissToast(id), kind === "error" ? 8000 : 4000);
  },
  dismissToast: (id) => set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
  fail: (e) => get().toast("error", errMessage(e)),

  refreshAll: async () => {
    const [projects, tasks, panes, agents, repoSets, repoRules] = await Promise.all([
      api.listProjects(), api.listTasks(), api.listPanes(), api.listAgents(),
      api.listRepoSets(), api.listRepoRules(),
    ]);
    set((s) => ({
      projects, tasks, panes, agents, repoSets, repoRules,
      selectedTask: stillThere(tasks, s.selectedTask),
    }));
    await get().refreshSettings();
  },

  refreshRepos: async () => {
    const [projects, repoSets, repoRules] = await Promise.all([
      api.listProjects(), api.listRepoSets(), api.listRepoRules(),
    ]);
    set({ projects, repoSets, repoRules });
  },

  refreshTasks: async () => {
    const tasks = await api.listTasks();
    set((s) => ({ tasks, selectedTask: stillThere(tasks, s.selectedTask) }));
  },
  refreshPanes: async () => set({ panes: await api.listPanes() }),

  refreshSettings: async () => {
    const settings = await api.getSettings();
    set({ settings });
    if (settings.jira_connected && get().issues.length === 0) {
      void get().refreshIssues();
    }
  },

  refreshIssues: async () => {
    if (!get().settings?.jira_connected) return;
    set({ issuesLoading: true });
    try {
      const [page, issueTypes] = await Promise.all([
        api.jiraIssues(),
        // Types rarely change and are cached in the backend; a failure here
        // must not stop the issues themselves from showing.
        api.jiraIssueTypes().catch(() => get().issueTypes),
      ]);
      set({ issues: page.issues, issuesTruncated: page.more, issueTypes });
    } catch (e) {
      get().fail(e);
    } finally {
      set({ issuesLoading: false });
    }
  },
}));

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
