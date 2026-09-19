export interface Project {
  id: string;
  name: string;
  path: string;
  default_branch: string;
  /** One group per repo; null means ungrouped. */
  group: string | null;
}

export interface RepoSet {
  id: string;
  name: string;
  project_ids: string[];
}

export type MatchKind = "component" | "label";

export interface RepoRule {
  id: string;
  kind: MatchKind;
  value: string;
  project_ids: string[];
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
}

export interface CheckoutView extends Checkout {
  project_name: string;
  status: WorktreeStatus | null;
  exists: boolean;
  /** Files differing from the base branch — what the Diff tab lists. */
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
}

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
  changed: number;
  error: string | null;
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

export interface WorktreeEntry {
  path: string;
  branch: string | null;
  head: string | null;
  locked: boolean;
}
