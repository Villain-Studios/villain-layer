export interface Project {
  id: string;
  name: string;
  path: string;
  default_branch: string;
  /** One group per repo; null means ungrouped. */
  group: string | null;
  /** The app's own copy of the repository, which task worktrees belong to. */
  store: string | null;
  /** How Update from base updates branches here: set in Repos, or the way it
   *  was last updated. Absent: guessed from its history (UPD-7). */
  update_by?: UpdateBy | null;
}

/** How a registered repository is doing (REPO-5). */
export interface RepoHealth {
  project_id: string;
  /** `missing`: nothing at its path; `not_repo`: no longer a git repository;
   *  `other`: a different repository from the one the app's copy was made of. */
  clone: "ok" | "missing" | "not_repo" | "other";
  origin: string | null;
  store: string | null;
  /** When a Sync last reached origin, in seconds since the epoch. */
  synced_at: number | null;
  /** The clone's default branch against origin's. */
  behind: number | null;
  ahead: number | null;
  /** A folder that looks like this repository, when the clone is missing. */
  found: string | null;
  /** How its history says the team updates branches, and why. */
  update_guess: UpdateBy | null;
  update_reason: string | null;
}

/** One repository's row from Sync (REPO-7). */
export interface Synced {
  project_id: string;
  repo: string;
  ok: boolean;
  detail: string;
}

/** Something Clean up would remove (REPO-8). */
export interface CleanupItem {
  id: string;
  kind: "worktree" | "folder" | "clone_branch" | "store_branch" | "records" | "store";
  repo: string | null;
  title: string;
  detail: string;
  /** safe: loses nothing, pre-selected. risky: offered, never pre-selected.
   *  blocked: not removable here. */
  verdict: "safe" | "risky" | "blocked";
}

export interface Cleaned {
  id: string;
  ok: boolean;
  detail: string;
}

export interface RepoSuggestion {
  project_ids: string[];
  reason: string | null;
}

/** One ticket's worth of work, spanning one or more repositories. */
export interface Task {
  id: string;
  name: string;
  root: string;
  branch: string;
  issue_key: string | null;
  issue_url: string | null;
  created_at: string;
  /** The last stage its ticket was moved for (TKT-8). */
  ticket_stage?: "review" | "merged" | null;
}

/** One repository's worktree within a task. */
export interface Checkout {
  id: string;
  task_id: string;
  project_id: string;
  path: string;
  base: string;
}

export interface WorktreeStatus {
  branch: string;
  ahead: number;
  behind: number;
  staged: number;
  unstaged: number;
  untracked: number;
  conflicted: number;
  /** Distinct paths with any uncommitted change. */
  dirty_files: number;
}

export interface CheckoutView extends Checkout {
  project_name: string;
  status: WorktreeStatus | null;
  exists: boolean;
  /** The folder is there but git cannot read it, and why — its repository forgot it, say. */
  broken: string | null;
  /** Files with uncommitted changes — what the Diff tab lists by default. */
  changed: number;
}

export interface TaskView extends Task {
  checkouts: CheckoutView[];
  pane_count: number;
}

export type PaneKind = "agent" | "shell";

export interface PaneInfo {
  id: string;
  task_id: string;
  /** null means the pane is rooted at the task root, seeing every repo. */
  checkout_id: string | null;
  kind: PaneKind;
  title: string;
  agent_id: string | null;
  cwd: string;
  running: boolean;
  exit_code: number | null;
  started_at: string;
  last_output_at: string;
  /** Something is waiting on you: "usage_limit" or "trust_prompt". */
  notice: string | null;
  /**
   * What the agent is doing: from its own reports — hooks, a plugin or its
   * window title, for every CLI offered — and guessed from output only when
   * those stop arriving. "done" is finished and not yet looked at; once seen
   * it is "idle".
   */
  activity: "working" | "asking" | "done" | "idle";
  /** Since when. Not `last_output_at`: an idle Claude Code repaints every few seconds. */
  activity_since: string;
}

/** What a terminal coming on screen missed. See `pty_attach`. */
export interface Catchup {
  /** Base64 output after the point asked from. */
  data: string;
  /** Where `data` ends in the pane's output; ask from here next time. */
  end: number;
  /** The point asked from is gone: clear the terminal before drawing `data`. */
  reset: boolean;
}

export interface AgentStatus {
  id: string;
  name: string;
  program: string;
  installed: boolean;
  path: string | null;
}

export interface Resumable {
  agent_id: string;
  name: string;
  sessions: number;
  /** Unix seconds of the newest transcript. */
  last_active: number | null;
}

/** Which comparison the Diff tab is showing. */
export type DiffScope = "uncommitted" | "branch";

/** One commit on a task branch, for the Diff view's commit picker. */
export interface CommitInfo {
  sha: string;
  short: string;
  subject: string;
}

export interface RepoCommits {
  checkout_id: string;
  repo: string;
  commits: CommitInfo[];
}

export interface ChangedFile {
  path: string;
  additions: number;
  deletions: number;
  binary: boolean;
  origin: string;
  checkout_id: string;
  repo: string;
}

/** What finishing a task did. */
export interface Finished {
  /** Any row not ok means the task was kept and nothing after was done. */
  repos: RepoResult[];
  branches: RepoResult[];
  ticket_moved: boolean;
  ticket_error: string | null;
}

/** How Update from base takes in the base: merged in, or rebased onto. */
export type UpdateBy = "merge" | "rebase";

/** What bringing one repository up to date with its base came to. */
export interface RepoUpdate {
  checkout_id: string;
  repo: string;
  base: string;
  outcome: "up_to_date" | "updated" | "conflicts" | "failed";
  /** Commits the base had that the branch did not. */
  commits: number;
  conflicts: string[];
  detail: string;
}

export interface RepoResult {
  checkout_id: string;
  repo: string;
  ok: boolean;
  detail: string;
}

export interface JiraIssue {
  key: string;
  summary: string;
  description: string;
  status: string;
  status_id: string;
  status_category: string;
  issue_type: string;
  priority: string | null;
  assignee: string | null;
  labels: string[];
  components: string[];
  epic_key: string | null;
  epic_summary: string | null;
  url: string;
  /** When it was filed, RFC 3339 (TKT-11). */
  created: string | null;
}

/** A field a project insists on before it will accept a new issue. */
export interface CreateField {
  id: string;
  name: string;
  required: boolean;
  /** "array" when Jira expects several values. */
  kind: string;
  allowed: { id: string; name: string }[];
}

/** A repository added to a task, and how many running agents were told. */
export interface AddedRepo extends Checkout {
  told: number;
}

/** A task, plus the status its ticket was moved to on the way. */
export interface Started extends Task {
  moved: string | null;
}

/** A page of search results, and whether Jira still had more to give. */
export interface JiraPage {
  issues: JiraIssue[];
  more: boolean;
}

export interface JiraIssueType {
  id: string;
  name: string;
  subtask: boolean;
  /** 1+ is epic-level, 0 a standard issue, -1 a sub-task. */
  hierarchy_level: number;
  /** Jira's own icon, inlined as a data URI by the backend. */
  icon: string | null;
}

export interface JiraTransition {
  id: string;
  name: string;
  to_status: string;
  to_id: string;
  /** "new", "indeterminate" or "done" — the same on every site, unlike the names. */
  to_category: string;
}

/** A status a project's tickets can be in. */
export interface ProjectStatus {
  id: string;
  name: string;
  category: string;
}

export interface FlowStatus {
  id: string;
  name: string;
}

/** Where a Jira project's tickets go as the work moves (TKT-8). */
export interface TicketFlow {
  review: FlowStatus | null;
  merged: FlowStatus | null;
}

/** What the PR sweep did about one task's ticket. */
export interface TicketMove {
  key: string;
  stage: "review" | "merged";
  moved_to: string | null;
  /** No status is chosen for this stage in the ticket's project yet. */
  unchosen: boolean;
  error: string | null;
}

export interface PullRequest {
  number: number;
  title: string;
  state: string;
  draft: boolean;
  author: string;
  head: string;
  /** The commit the branch was at on GitHub; for a merged PR, what landed. */
  head_sha: string;
  base: string;
  url: string;
  mergeable_state: string | null;
  merged: boolean;
  /** Conversation comments and inline review comments. */
  comments: number;
  review_comments: number;
}

export interface Review {
  author: string;
  /** APPROVED, CHANGES_REQUESTED, COMMENTED or DISMISSED. */
  state: string;
  submitted_at: string | null;
  url: string;
}

/** What a PR's reviews add up to. */
export type Verdict = "approved" | "changes_requested" | "commented" | "none";

export interface CheckRun {
  name: string;
  status: string;
  conclusion: string | null;
  url: string | null;
}

export interface CheckoutPr {
  checkout_id: string;
  repo: string;
  pr: PullRequest | null;
  checks: CheckRun[];
  reviews: Review[];
  /** Earlier pull requests from this branch, newest first. */
  past: PullRequest[];
  verdict: Verdict;
  /** Where this repo's next PR is opened against; may differ from pr.base. */
  base: string;
  /** Files changed on the branch — or, once its PR has merged, since what it landed. */
  changed: number;
  error: string | null;
}

export interface RepoBranchFacts {
  checkout_id: string;
  repo: string;
  base: string;
  /** Abbreviated commit the branch is measured from. */
  baseline: string;
  /** True when that is the branch point recorded at creation, not a merge base. */
  baseline_recorded: boolean;
  commits: number;
  unpushed: number;
  has_remote: boolean;
}

/** One comment, review or reply on a pull request. */
export interface FeedbackNote {
  author: string;
  /** Written by an app — a coverage bot, a linter — not a person. */
  bot: boolean;
  /** For a review: APPROVED, CHANGES_REQUESTED or COMMENTED. */
  state: string | null;
  body: string;
  url: string;
  at: string | null;
}

/** A line of the code under a review thread; `n` is null on the other side. */
export interface CodeLine {
  n: number | null;
  op: "+" | "-" | " ";
  text: string;
}

export interface ReviewThread {
  path: string;
  line: number | null;
  /** Where a comment on a range of lines begins. */
  start_line: number | null;
  code: CodeLine[];
  resolved: boolean;
  /** The code under it has changed since. */
  outdated: boolean;
  url: string;
  comments: FeedbackNote[];
}

export interface FailedCheck {
  name: string;
  conclusion: string;
  url: string | null;
  summary: string;
  /** The end of the job's log, for a GitHub Actions job. */
  log: string | null;
}

/** What reviewers and CI have said on one repository's open PR. */
export interface RepoFeedback {
  checkout_id: string;
  repo: string;
  number: number;
  title: string;
  url: string;
  /** Who opened the PR. */
  author: string;
  threads: ReviewThread[];
  reviews: FeedbackNote[];
  comments: FeedbackNote[];
  checks: FailedCheck[];
  error: string | null;
}

/** A piece of feedback chosen to go to an agent. */
export type FeedbackItem =
  | {
      kind: "thread"; checkout_id: string; path: string; line: number | null;
      start_line: number | null; code: CodeLine[]; outdated: boolean; resolved: boolean; url: string; comments: { author: string; body: string }[];
    }
  | { kind: "review"; checkout_id: string; author: string; state: string | null; body: string; url: string }
  | { kind: "comment"; checkout_id: string; author: string; body: string; url: string }
  | {
      kind: "check"; checkout_id: string; name: string; conclusion: string;
      url: string | null; summary: string; log: string | null;
    };

export interface TaskPrs {
  task_id: string;
  rows: CheckoutPr[];
  ticket: TicketMove | null;
}

export interface JiraConfig {
  base_url: string;
  email: string;
  project_key: string | null;
  jql: string | null;
  /** Keyed by Jira project key. */
  flow: Record<string, TicketFlow>;
}

export interface GithubConfig {
  api_url: string;
  web_url: string;
  /** `@fe` or `org/fe`. Null lists only reviews requested of you. */
  review_team: string | null;
}

/** A pull request waiting on a person or a team. */
export interface ReviewRequest {
  repo: string;
  number: number;
  title: string;
  url: string;
  author: string;
  draft: boolean;
  updated_at: string;
}

export interface TeamReviews {
  /** `org/slug`. */
  slug: string;
  name: string;
  prs: ReviewRequest[];
  more: boolean;
  error: string | null;
}

export interface ReviewQueue {
  mine: ReviewRequest[];
  mine_more: boolean;
  /** Null when no review team is configured. */
  team: TeamReviews | null;
}

export interface SlackConfig {
  channel: string;
  enabled: boolean;
  notify_on_done: boolean;
  notify_on_prs: boolean;
  allow_agent_posts: boolean;
}

export interface UiPrefs {
  scale: number;
  terminal_font_size: number;
  restore_panes: boolean;
  trust_agent_dirs: boolean;
  sync_jira_status: boolean;
  /** A banner for a new review or an assigned ticket, while the window is in the background. */
  system_notifications: boolean;
  /** A banner when an agent stops to wait on you while the window is away, and the dock count. */
  notify_waiting_agents: boolean;
  agents_read_panes: boolean;
}

export interface Settings {
  ui: UiPrefs;
  jira: JiraConfig | null;
  github: GithubConfig | null;
  slack: SlackConfig | null;
  worktree_root: string;
  worktree_root_is_default: boolean;
  /** What Update from base does unless told otherwise: the last one used. */
  jira_connected: boolean;
  github_connected: boolean;
  slack_connected: boolean;
}

/**
 * What a click on a banner, a toast or a message opens (NOTE-4), as
 * `target.rs` writes it; `lib/target.ts` routes it. Banners hand it back in
 * `system-notify-click`.
 */
export type Target =
  | "reviews"
  | "tickets"
  | "work"
  | "chat"
  | `task:${string}`
  | `pane:${string}:${string}`
  | `pr:${string}`
  | `ticket:${string}`
  | `review:${string}`;

export type MessageKind = "agent" | "review" | "ticket" | "pr" | "error" | "notice";
export type MessageLevel = "info" | "success" | "error";

/** One row of the message center (`messages.rs`), newest first. */
export interface Message {
  id: number;
  /** Unix milliseconds of the latest time it was said. */
  at: number;
  kind: MessageKind;
  level: MessageLevel;
  title: string;
  body: string;
  /** Where a click goes (NOTE-4). A string: an older build's may not be a `Target` this one knows. */
  target: string | null;
  read: boolean;
  /** Said this many times while unread, within ten minutes. */
  count: number;
}

/** Queued before the UI was listening — drained once on boot. */
export interface AppNotice {
  kind: string;
  text: string;
}

export interface ReviewComment {
  path: string;
  line: number;
  body: string;
  code?: string | null;
  repo?: string | null;
}

export interface FoundRepo {
  path: string;
  name: string;
  branch: string;
  registered: boolean;
}
