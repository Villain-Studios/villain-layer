/**
 * What the mock backend answers with. Typed against `lib/types.ts` on
 * purpose: when a Rust struct changes and its TypeScript mirror follows, this
 * file stops compiling, and the harness cannot drift into showing a UI the
 * real backend would never produce.
 *
 * Neutral names only (acme, ACME-1) — nothing here is anyone's real Jira or
 * GitHub.
 */
import type {
  AgentStatus,
  ChangedFile,
  CreateField,
  JiraIssue,
  JiraIssueType,
  JiraTransition,
  PaneInfo,
  Project,
  RepoFeedback,
  ReviewQueue,
  Settings,
  TaskPrs,
  TaskView,
  UiPrefs,
} from "../lib/types";

export interface World {
  projects: Project[];
  tasks: TaskView[];
  panes: PaneInfo[];
  agents: AgentStatus[];
  settings: Settings;
  prs: TaskPrs[];
  reviews: ReviewQueue;
  issues: JiraIssue[];
  issueTypes: JiraIssueType[];
  transitions: JiraTransition[];
  requiredFields: CreateField[];
  changed: ChangedFile[];
  feedback: RepoFeedback[];
  /** What each pane's terminal shows when it comes on screen. */
  output: Record<string, string>;
}

const now = Date.now();
export const ago = (seconds: number) => new Date(now - seconds * 1000).toISOString();

const ui: UiPrefs = {
  scale: 1,
  terminal_font_size: 13,
  restore_panes: true,
  trust_agent_dirs: true,
  sync_jira_status: true,
  system_notifications: true,
  notify_waiting_agents: true,
  agents_read_panes: false,
};

const disconnected: Settings = {
  ui,
  jira: null,
  github: null,
  slack: null,
  worktree_root: "/Users/you/.villain-worktrees",
  worktree_root_is_default: true,
  update_by: "merge",
  jira_connected: false,
  github_connected: false,
  slack_connected: false,
};

const agents: AgentStatus[] = [
  { id: "claude", name: "Claude Code", program: "claude", installed: true, path: "/usr/local/bin/claude" },
  { id: "copilot", name: "GitHub Copilot CLI", program: "copilot", installed: true, path: "/usr/local/bin/copilot" },
  { id: "opencode", name: "OpenCode", program: "opencode", installed: false, path: null },
  { id: "gemini", name: "Gemini CLI", program: "gemini", installed: true, path: "/usr/local/bin/gemini" },
];

const issueTypes: JiraIssueType[] = [
  { id: "1", name: "Epic", subtask: false, hierarchy_level: 1, icon: null },
  { id: "2", name: "Story", subtask: false, hierarchy_level: 0, icon: null },
  { id: "3", name: "Task", subtask: false, hierarchy_level: 0, icon: null },
  { id: "4", name: "Bug", subtask: false, hierarchy_level: 0, icon: null },
  { id: "5", name: "Sub-task", subtask: true, hierarchy_level: -1, icon: null },
];

function issue(key: string, summary: string, status: string, category: string, epic: string | null): JiraIssue {
  return {
    key,
    summary,
    description: `What ${key} is about, as the ticket says it.`,
    status,
    status_category: category,
    issue_type: "Story",
    priority: "Medium",
    assignee: "You",
    labels: [],
    components: [],
    epic_key: epic,
    epic_summary: epic ? "Sign-in overhaul" : null,
    url: `https://acme.atlassian.net/browse/${key}`,
  };
}

const projects: Project[] = [
  { id: "p-api", name: "api", path: "/Users/you/code/api", default_branch: "main", group: "platform" },
  { id: "p-web", name: "web", path: "/Users/you/code/web", default_branch: "main", group: "platform" },
  { id: "p-infra", name: "infra", path: "/Users/you/code/infra", default_branch: "main", group: null },
];

const clean = { ahead: 0, behind: 0, staged: 0, unstaged: 0, untracked: 0, conflicted: 0, dirty_files: 0 };

const tasks: TaskView[] = [
  {
    id: "t-login",
    name: "ACME-123 Fix login race",
    root: "/Users/you/.villain-worktrees/ACME-123",
    branch: "ACME-123",
    issue_key: "ACME-123",
    issue_url: "https://acme.atlassian.net/browse/ACME-123",
    created_at: ago(2 * 86400),
    pane_count: 2,
    checkouts: [
      {
        id: "c-login-api", task_id: "t-login", project_id: "p-api", project_name: "api",
        path: "/Users/you/.villain-worktrees/ACME-123/api", base: "main", exists: true, changed: 2,
        status: { ...clean, branch: "ACME-123", ahead: 2, behind: 3, unstaged: 1, untracked: 1, dirty_files: 2 },
      },
      {
        id: "c-login-web", task_id: "t-login", project_id: "p-web", project_name: "web",
        path: "/Users/you/.villain-worktrees/ACME-123/web", base: "main", exists: true, changed: 0,
        status: { ...clean, branch: "ACME-123", ahead: 1 },
      },
    ],
  },
  {
    id: "t-audit",
    name: "ACME-130 Audit log page",
    root: "/Users/you/.villain-worktrees/ACME-130",
    branch: "ACME-130",
    issue_key: "ACME-130",
    issue_url: "https://acme.atlassian.net/browse/ACME-130",
    created_at: ago(6 * 86400),
    pane_count: 1,
    checkouts: [
      {
        id: "c-audit-web", task_id: "t-audit", project_id: "p-web", project_name: "web",
        path: "/Users/you/.villain-worktrees/ACME-130/web", base: "main", exists: true, changed: 0,
        status: { ...clean, branch: "ACME-130" },
      },
    ],
  },
];

function pane(
  id: string, task: string, kind: PaneInfo["kind"], title: string, agent: string | null,
  activity: PaneInfo["activity"], since: number, extra: Partial<PaneInfo> = {},
): PaneInfo {
  return {
    id, task_id: task, checkout_id: null, kind, title, agent_id: agent,
    cwd: `/Users/you/.villain-worktrees/${task}`, running: true, exit_code: null,
    started_at: ago(3600), last_output_at: ago(2), notice: null,
    activity, activity_since: ago(since), ...extra,
  };
}

const panes: PaneInfo[] = [
  pane("pane-claude", "t-login", "agent", "Claude Code", "claude", "asking", 40),
  pane("pane-shell", "t-login", "shell", "Shell · api", null, "idle", 3600, { checkout_id: "c-login-api" }),
  pane("pane-copilot", "t-audit", "agent", "GitHub Copilot CLI", "copilot", "done", 300),
  pane("pane-chat", "chat", "agent", "Claude Code", "claude", "working", 12),
];

const pr = (number: number, repo: string, merged: boolean) => ({
  number,
  title: repo === "api" ? "Fix login race" : "Audit log page",
  state: merged ? "closed" : "open",
  draft: false,
  author: "you",
  head: "ACME-123",
  head_sha: "4f2c9e1",
  base: "main",
  url: `https://github.com/acme/${repo}/pull/${number}`,
  mergeable_state: merged ? null : "clean",
  merged,
  comments: 2,
  review_comments: 3,
});

const prs: TaskPrs[] = [
  {
    task_id: "t-login",
    rows: [
      {
        checkout_id: "c-login-api", repo: "api", pr: pr(42, "api", false),
        checks: [
          { name: "build", status: "completed", conclusion: "failure", url: "https://github.com/acme/api/actions/runs/1" },
          { name: "lint", status: "completed", conclusion: "success", url: null },
        ],
        reviews: [{ author: "ana", state: "CHANGES_REQUESTED", submitted_at: ago(1800), url: "" }],
        past: [], verdict: "changes_requested", base: "main", changed: 4, error: null,
      },
      {
        checkout_id: "c-login-web", repo: "web", pr: null, checks: [], reviews: [], past: [],
        verdict: "none", base: "main", changed: 1, error: null,
      },
    ],
  },
  {
    task_id: "t-audit",
    rows: [
      {
        checkout_id: "c-audit-web", repo: "web", pr: pr(7, "web", true), checks: [], reviews: [],
        past: [], verdict: "approved", base: "main", changed: 0, error: null,
      },
    ],
  },
];

const feedback: RepoFeedback[] = [
  {
    checkout_id: "c-login-api", repo: "api", number: 42, title: "Fix login race",
    url: "https://github.com/acme/api/pull/42", author: "you",
    threads: [
      {
        path: "src/auth.ts", line: 42, resolved: false, outdated: false, url: "https://github.com/acme/api/pull/42#t1",
        comments: [{ author: "ana", bot: false, state: null, body: "The retry counter starts at 1, so this makes one attempt fewer than configured.", url: "", at: ago(1800) }],
      },
    ],
    reviews: [{ author: "ana", bot: false, state: "CHANGES_REQUESTED", body: "Needs a test for the retry path.", url: "", at: ago(1800) }],
    comments: [{ author: "codecov", bot: true, state: null, body: "Coverage dropped 0.4%.", url: "", at: ago(1700) }],
    checks: [{
      name: "build", conclusion: "failure", url: "https://github.com/acme/api/actions/runs/1", summary: "",
      log: "src/auth.test.ts:\n  ✗ retries the configured number of times\n    expected 3, got 2",
    }],
    error: null,
  },
];

const say = (lines: string[]) => lines.join("\r\n") + "\r\n";

/** Everything connected and in use: the view most screens are designed for. */
function busy(): World {
  return {
    projects,
    tasks,
    panes,
    agents,
    settings: {
      ...disconnected,
      jira: { base_url: "https://acme.atlassian.net", email: "you@acme.test", project_key: "ACME", jql: null },
      github: { api_url: "https://api.github.com", web_url: "https://github.com", review_team: null },
      slack: { channel: "#dev", enabled: true, notify_on_done: true, notify_on_prs: true, allow_agent_posts: true },
      jira_connected: true,
      github_connected: true,
      slack_connected: true,
    },
    prs,
    reviews: {
      mine: [{ repo: "acme/web", number: 88, title: "Speed up the search box", url: "https://github.com/acme/web/pull/88", author: "bo", draft: false, updated_at: ago(5400) }],
      mine_more: false,
      team: null,
    },
    issues: [
      issue("ACME-123", "Fix login race", "In Progress", "indeterminate", "ACME-100"),
      issue("ACME-140", "Remember the last workspace", "To Do", "new", "ACME-100"),
      issue("ACME-141", "Rate-limit password resets", "To Do", "new", null),
    ],
    issueTypes,
    transitions: [
      { id: "11", name: "Start", to_status: "In Progress", to_category: "indeterminate" },
      { id: "31", name: "Done", to_status: "Done", to_category: "done" },
    ],
    requiredFields: [
      { id: "components", name: "Components", required: true, kind: "array", allowed: [{ id: "c1", name: "Backend" }, { id: "c2", name: "Web" }] },
    ],
    changed: [
      { path: "src/auth.ts", additions: 12, deletions: 3, binary: false, origin: "tracked", checkout_id: "c-login-api", repo: "api" },
      { path: "src/auth.test.ts", additions: 40, deletions: 0, binary: false, origin: "untracked", checkout_id: "c-login-api", repo: "api" },
    ],
    feedback,
    output: {
      "pane-claude": say([
        "\x1b[1m✻ Claude Code\x1b[0m",
        "",
        "> Fix the login race described in ACME-123",
        "",
        "I'll add a lock around the session refresh in src/auth.ts.",
        "",
        "\x1b[33mDo you want to make this edit to src/auth.ts?\x1b[0m",
        "❯ 1. Yes",
        "  2. No, and tell Claude what to do differently",
      ]),
      "pane-shell": say(["you@mac api % git status --short", " M src/auth.ts", "?? src/auth.test.ts", "you@mac api % "]),
      "pane-copilot": say(["● Done. The audit log page is behind the feature flag."]),
      "pane-chat": say(["> What is left on ACME-100?", "", "Looking at the epic's tickets…"]),
    },
  };
}

/** First launch: nothing added, nothing connected. */
function empty(): World {
  return {
    projects: [], tasks: [], panes: [], agents, settings: disconnected, prs: [],
    reviews: { mine: [], mine_more: false, team: null },
    issues: [], issueTypes: [], transitions: [], requiredFields: [], changed: [], feedback: [], output: {},
  };
}

export const SCENARIOS: Record<string, () => World> = { busy, empty };
