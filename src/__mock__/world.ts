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
  CleanupItem,
  CreateField,
  JiraIssue,
  JiraIssueType,
  JiraTransition,
  Message,
  PaneInfo,
  Project,
  RepoFeedback,
  RepoHealth,
  ReviewQueue,
  Settings,
  TaskPrs,
  TaskView,
  UiPrefs,
} from "../lib/types";

export interface World {
  projects: Project[];
  health: RepoHealth[];
  cleanup: CleanupItem[];
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
  /** The message center's log, newest first. */
  messages: Message[];
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

/** Shaped like a real workflow: Review shares In Progress's category, and two statuses are done. */
const STATUS_IDS: Record<string, string> = { "To Do": "1", "In Progress": "3", Review: "10322", Done: "10002", Cancelled: "10016" };

function issue(key: string, summary: string, status: string, category: string, epic: string | null): JiraIssue {
  return {
    key,
    summary,
    description: `What ${key} is about, as the ticket says it.`,
    status,
    status_id: STATUS_IDS[status] ?? "0",
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

const copies = "/Users/you/.villain-worktrees/.repos";
const projects: Project[] = [
  { id: "p-api", name: "api", path: "/Users/you/code/api", default_branch: "main", group: "platform", store: `${copies}/api.git` },
  { id: "p-web", name: "web", path: "/Users/you/code/web", default_branch: "main", group: "platform", store: `${copies}/web.git` },
  { id: "p-infra", name: "infra", path: "/Users/you/code/infra", default_branch: "main", group: null, store: null },
];

const hourAgo = Math.round(Date.now() / 1000) - 3600;

/** api is behind, web has a commit of its own on main, infra has moved. */
const health: RepoHealth[] = [
  { project_id: "p-api", clone: "ok", origin: "git@github.com:acme/api.git", store: `${copies}/api.git`, synced_at: hourAgo, behind: 3, ahead: 0, found: null, update_guess: "rebase", update_reason: "main is a straight line of commits: branches are rebased onto it" },
  { project_id: "p-web", clone: "ok", origin: "git@github.com:acme/web.git", store: `${copies}/web.git`, synced_at: hourAgo, behind: 0, ahead: 1, found: null, update_guess: "merge", update_reason: "pull requests land on main as merge commits" },
  { project_id: "p-infra", clone: "missing", origin: null, store: null, synced_at: null, behind: null, ahead: null, found: "/Users/you/code/ops/infra", update_guess: null, update_reason: null },
];

/** One of each thing Clean up finds, in each verdict it can have. */
const cleanup: CleanupItem[] = [
  { id: "folder:ACME-90", kind: "folder", repo: null, title: "ACME-90", verdict: "safe", detail: "No task uses it. It holds 2 files the app wrote." },
  { id: "folder:ACME-77", kind: "folder", repo: null, title: "ACME-77", verdict: "blocked", detail: "No task uses it, but api: 3 uncommitted changes." },
  { id: "clone_branch:api:ACME-123", kind: "clone_branch", repo: "api", title: "ACME-123", verdict: "safe", detail: "A task works on this branch in the app's copy, which has every commit on it. The task does not need this one." },
  { id: "store_branch:api:ACME-88", kind: "store_branch", repo: "api", title: "ACME-88", verdict: "risky", detail: "No task uses it, but 2 commits on it are on no origin branch. A branch squash-merged and then deleted on origin looks like this too." },
  { id: "records:web", kind: "records", repo: "web", title: "Records of deleted worktrees", verdict: "safe", detail: "Git still lists 1 worktree whose folder is gone." },
  { id: "store:old-tool.git", kind: "store", repo: null, title: "old-tool.git", verdict: "safe", detail: "No repo uses it, and origin has everything in it." },
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
        path: "/Users/you/.villain-worktrees/ACME-123/api", base: "main", exists: true, broken: null, changed: 2,
        status: { ...clean, branch: "ACME-123", ahead: 2, behind: 3, unstaged: 1, untracked: 1, dirty_files: 2 },
      },
      {
        id: "c-login-web", task_id: "t-login", project_id: "p-web", project_name: "web",
        path: "/Users/you/.villain-worktrees/ACME-123/web", base: "main", exists: true, broken: null, changed: 0,
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
        path: "/Users/you/.villain-worktrees/ACME-130/web", base: "main", exists: true, broken: null, changed: 0,
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
    ticket: null,
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
    ticket: null,
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
        path: "src/auth.ts", line: 42, start_line: null, resolved: false, outdated: false, url: "https://github.com/acme/api/pull/42#t1",
        code: [
          { n: 39, op: " ", text: "export async function withRetry<T>(run: () => Promise<T>, tries: number) {" },
          { n: null, op: "-", text: "  let attempt = 0;" },
          { n: 40, op: "+", text: "  let attempt = 1;" },
          { n: 41, op: " ", text: "  for (;;) {" },
          { n: 42, op: " ", text: "    if (attempt >= tries) throw new Error(\"gave up\");" },
        ],
        comments: [
          { author: "ana", bot: false, state: null, body: "The retry counter starts at `1`, so this makes **one attempt fewer** than configured.\n\n```ts\nlet attempt = 0;\n```", url: "", at: ago(1800) },
          { author: "you", bot: false, state: null, body: "Good catch — fixed in the next push.", url: "", at: ago(1700) },
        ],
      },
      {
        path: "src/table.scss", line: 20, start_line: 16, resolved: false, outdated: false, url: "https://github.com/acme/api/pull/42#t2",
        code: [
          { n: 16, op: "+", text: ".mat-column-id {" },
          { n: 17, op: "+", text: "  font-size: var(--text-xs);" },
          { n: 18, op: "+", text: "  line-height: var(--text-xs--line-height);" },
          { n: 19, op: "+", text: "  word-break: break-all;" },
          { n: 20, op: "+", text: "}" },
        ],
        comments: [
          { author: "copilot-pull-request-reviewer", bot: true, state: null, url: "", at: ago(900), body: "This selector matches both the header and data cells. Scope these rules to `td.mat-column-id`." },
        ],
      },
      {
        path: "src/retry.ts", line: 8, start_line: null, resolved: true, outdated: true, url: "https://github.com/acme/api/pull/42#t3",
        code: [{ n: 8, op: "+", text: "const DELAY = 100;" }],
        comments: [
          { author: "copilot-pull-request-reviewer", bot: true, state: null, url: "", at: ago(3600), body: "A fixed delay retries in lockstep; consider jitter." },
        ],
      },
    ],
    reviews: [{ author: "ana", bot: false, state: "CHANGES_REQUESTED", body: "Needs a test for the retry path.", url: "", at: ago(1800) }],
    comments: [
      {
        author: "github-actions", bot: true, state: null, url: "", at: ago(360),
        body: "<!-- deploy-comment -->\n### 🚀 Preview deployed\n\n**Build:** `a1b2c3d`\n\n**Available at:**\n- https://acme-123.web.preview.acme.test",
      },
      {
        author: "codecov", bot: true, state: null, url: "", at: ago(1700),
        body: "## Coverage dropped 0.4%\n\n| File | Coverage | Δ |\n|:--|--:|--:|\n| src/auth.ts | 81.2% | -2.1% |\n| src/retry.ts | 94.0% | +0.3% |\n\n<details><summary>Details</summary>\n\n- [x] tests ran\n- [ ] integration suite\n</details>",
      },
    ],
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
    health,
    cleanup,
    tasks,
    panes,
    agents,
    settings: {
      ...disconnected,
      jira: { base_url: "https://acme.atlassian.net", email: "you@acme.test", project_key: "ACME", jql: null, flow: {} },
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
      { id: "11", name: "Start", to_status: "In Progress", to_id: "3", to_category: "indeterminate" },
      { id: "4", name: "Request Review", to_status: "Review", to_id: "10322", to_category: "indeterminate" },
      { id: "31", name: "Move to Done", to_status: "Done", to_id: "10002", to_category: "done" },
      { id: "81", name: "Cancel", to_status: "Cancelled", to_id: "10016", to_category: "done" },
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
    messages: messages(),
  };
}

/** One of each kind the message center shows, and each way a click can land. */
function messages(): Message[] {
  const at = (seconds: number) => now - seconds * 1000;
  const m = (id: number, seconds: number, rest: Partial<Message> & Pick<Message, "kind" | "title">): Message => ({
    id, at: at(seconds), level: "info", body: "", target: null, read: false, count: 1, ...rest,
  });
  return [
    m(8, 40, { kind: "agent", level: "info", title: "ACME-123 Fix login race", body: "Claude Code is asking for your permission", target: "pane:t-login:pane-claude" }),
    m(7, 300, { kind: "pr", level: "success", title: "web #7 merged — ACME-130 Audit log page", target: "pr:t-audit" }),
    m(6, 900, { kind: "review", title: "Review requested", body: "acme/web#88 — Speed up the search box", target: "review:acme/web#88" }),
    m(5, 1500, { kind: "error", level: "error", title: "Jira did not answer: 503 Service Unavailable", count: 3 }),
    m(4, 3600, { kind: "ticket", title: "New ticket", body: "ACME-141 — Rate-limit password resets", target: "ticket:ACME-141" }),
    m(3, 7200, { kind: "agent", level: "error", title: "ACME-123 Fix login race", body: "Codex exited with 1", target: "pane:t-login:pane-from-last-launch" }),
    m(2, 86400, { kind: "agent", level: "success", title: "Chat", body: "Claude Code finished", target: "pane:chat:pane-chat", read: true }),
    m(1, 90000, { kind: "notice", title: "Moved 2 worktrees onto the app's own copies of their repositories.", read: true }),
  ];
}

/** First launch: nothing added, nothing connected. */
function empty(): World {
  return {
    projects: [], health: [], cleanup: [], tasks: [], panes: [], agents, settings: disconnected, prs: [],
    reviews: { mine: [], mine_more: false, team: null },
    issues: [], issueTypes: [], transitions: [], requiredFields: [], changed: [], feedback: [], output: {},
    messages: [],
  };
}

/** Busy, with ACME-123's worktrees cut off from their repositories (TASK-11). */
function unlinked(): World {
  const w = busy();
  for (const c of w.tasks[0].checkouts) {
    c.broken = `Git no longer knows this worktree: /Users/you/code/${c.project_name}/.git/worktrees/${c.project_name} is gone, usually because the repository was deleted or cloned again`;
    c.status = null;
    c.changed = 0;
  }
  w.changed = [];
  return w;
}

/** Busy, with the PR sweep reporting what it did about tickets (TKT-8). */
function tickets(): World {
  const w = busy();
  const [login, audit] = w.prs;
  login.ticket = { key: "ACME-123", stage: "review", moved_to: "Review", unchosen: false, error: null };
  const key = w.tasks.find((t) => t.id === audit.task_id)?.issue_key ?? "ACME-150";
  audit.ticket = { key, stage: "merged", moved_to: null, unchosen: true, error: null };
  return w;
}

export const SCENARIOS: Record<string, () => World> = { busy, empty, unlinked, tickets };
