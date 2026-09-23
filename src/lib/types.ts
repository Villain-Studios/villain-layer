export interface Project {
  id: string;
  name: string;
  path: string;
  default_branch: string;
  /** One group per repo; null means ungrouped. */
  group: string | null;
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
  status_category: string;
  issue_type: string;
  priority: string | null;
  assignee: string | null;
  labels: string[];
  components: string[];
  epic_key: string | null;
  epic_summary: string | null;
  url: string;
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
}

export interface PullRequest {
  number: number;
  title: string;
  state: string;
  draft: boolean;
  author: string;
  head: string;
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

export interface TaskPrs {
  task_id: string;
  rows: CheckoutPr[];
}

export interface JiraConfig {
  base_url: string;
  email: string;
  project_key: string | null;
  jql: string | null;
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
  agents_read_panes: boolean;
}

export interface Settings {
  ui: UiPrefs;
  jira: JiraConfig | null;
  github: GithubConfig | null;
  slack: SlackConfig | null;
  worktree_root: string;
  worktree_root_is_default: boolean;
  jira_connected: boolean;
  github_connected: boolean;
  slack_connected: boolean;
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
