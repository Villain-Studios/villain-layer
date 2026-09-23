import { invoke } from "@tauri-apps/api/core";
import type {
  AddedRepo,
  AgentStatus,
  Catchup,
  ChangedFile,
  CheckoutPr,
  CreateField,
  DiffScope,
  FoundRepo,
  JiraIssue,
  JiraIssueType,
  JiraPage,
  JiraTransition,
  PaneInfo,
  Project,
  RepoBranchFacts,
  RepoCommits,
  RepoResult,
  RepoSuggestion,
  Resumable,
  ReviewComment,
  ReviewQueue,
  Settings,
  SlackConfig,
  Started,
  Task,
  TaskPrs,
  TaskView,
  UiPrefs,
  AppNotice,
} from "./types";

export const api = {
  // projects
  listProjects: () => invoke<Project[]>("list_projects"),
  addProjects: (paths: string[], group?: string | null) =>
    invoke<Project[]>("add_projects", { paths, group: group ?? null }),
  setProjectGroup: (projectIds: string[], group: string | null) =>
    invoke<void>("set_project_group", { projectIds, group }),
  scanRepos: (root: string, maxDepth?: number) =>
    invoke<FoundRepo[]>("scan_repos", { root, maxDepth: maxDepth ?? null }),
  removeProject: (id: string) => invoke<void>("remove_project", { id }),
  projectBranches: (projectId: string) =>
    invoke<string[]>("project_branches", { projectId }),

  // tasks
  listTasks: (focus?: string | null) =>
    invoke<TaskView[]>("list_tasks", { focus: focus ?? null }),
  createTask: (req: {
    name: string;
    project_ids: string[];
    branch?: string | null;
    branch_suffix?: string | null;
    base?: string | null;
    issue_key?: string | null;
    issue_url?: string | null;
  }) => invoke<Task>("create_task", { req }),
  deleteTask: (id: string, force = false) =>
    invoke<RepoResult[]>("delete_task", { id, force }),
  addCheckout: (taskId: string, projectId: string) =>
    invoke<AddedRepo>("add_checkout", { taskId, projectId }),
  removeCheckout: (checkoutId: string, force = false) =>
    invoke<void>("remove_checkout", { checkoutId, force }),
  suggestRepos: (args: { issueKey?: string | null; epicKey?: string | null }) =>
    invoke<RepoSuggestion>("suggest_repos", {
      issueKey: args.issueKey ?? null,
      epicKey: args.epicKey ?? null,
    }),

  // panes
  listAgents: () => invoke<AgentStatus[]>("list_agents"),
  listPanes: (taskId?: string) => invoke<PaneInfo[]>("list_panes", { taskId: taskId ?? null }),
  spawnShell: (taskId: string, checkoutId?: string | null) =>
    invoke<PaneInfo>("spawn_shell", { taskId, checkoutId: checkoutId ?? null }),
  spawnAgent: (
    taskId: string, agentId: string,
    checkoutId?: string | null, prompt?: string | null, resume = false,
  ) => invoke<PaneInfo>("spawn_agent", {
    taskId, agentId, checkoutId: checkoutId ?? null, prompt: prompt ?? null, resume,
  }),
  resumableAgents: (taskId: string, checkoutId?: string | null) =>
    invoke<Resumable[]>("resumable_agents", { taskId, checkoutId: checkoutId ?? null }),
  spawnChat: (agentId: string, prompt?: string | null) =>
    invoke<PaneInfo>("spawn_chat", { agentId, prompt: prompt ?? null }),
  ptyWrite: (paneId: string, data: string) => invoke<void>("pty_write", { paneId, data }),
  ptyResize: (paneId: string, rows: number, cols: number) =>
    invoke<void>("pty_resize", { paneId, rows, cols }),
  ptyAttach: (paneId: string, since: number | null) =>
    invoke<Catchup>("pty_attach", { paneId, since }),
  ptyDetach: (paneId: string) => invoke<void>("pty_detach", { paneId }),
  closePane: (paneId: string) => invoke<void>("close_pane", { paneId }),
  killPane: (paneId: string) => invoke<void>("kill_pane", { paneId }),

  // diff + git
  diffFiles: (
    taskId: string,
    scope: DiffScope,
    commit?: { checkoutId: string; sha: string } | null,
  ) =>
    invoke<ChangedFile[]>("diff_files", {
      taskId,
      scope,
      commit: commit?.sha ?? null,
      checkoutId: commit?.checkoutId ?? null,
    }),
  diffFile: (
    checkoutId: string,
    path: string,
    scope: DiffScope,
    commitSha?: string | null,
  ) =>
    invoke<string>("diff_file", {
      checkoutId,
      path,
      scope,
      commit: commitSha ?? null,
    }),
  taskCommits: (taskId: string) =>
    invoke<RepoCommits[]>("task_commits", { taskId }),
  sendReview: (paneId: string, comments: ReviewComment[]) =>
    invoke<string>("send_review", { paneId, comments }),
  commitTask: (taskId: string, message: string) =>
    invoke<RepoResult[]>("commit_task", { taskId, message }),
  pushTask: (taskId: string) => invoke<RepoResult[]>("push_task", { taskId }),

  // jira
  jiraConnect: (
    baseUrl: string, email: string, token: string,
    projectKey?: string | null, jql?: string | null,
  ) => invoke<string>("jira_connect", { baseUrl, email, token, projectKey, jql }),
  jiraIssues: () => invoke<JiraPage>("jira_issues"),
  jiraIssueTypes: (refresh = false) =>
    invoke<JiraIssueType[]>("jira_issue_types", { refresh }),
  jiraEpics: (projectKey: string) =>
    invoke<JiraIssue[]>("jira_epics", { projectKey }),
  jiraTransitions: (key: string) => invoke<JiraTransition[]>("jira_transitions", { key }),
  jiraTransition: (key: string, transitionId: string) =>
    invoke<void>("jira_transition", { key, transitionId }),
  taskPrompt: (taskId: string, checkoutId?: string | null) =>
    invoke<string>("task_prompt", { taskId, checkoutId: checkoutId ?? null }),
  handoffPrompt: (paneId: string) => invoke<string>("handoff_prompt", { paneId }),
  draftPrDescription: (taskId: string) =>
    invoke<string>("draft_pr_description", { taskId }),
  optimizeIssueDescription: (args: {
    requestId: string;
    summary: string;
    description: string;
    kind: "epic" | "ticket";
  }) =>
    invoke<string>("optimize_issue_description", {
      requestId: args.requestId,
      summary: args.summary,
      description: args.description,
      kind: args.kind,
    }),
  requestPrDescription: (taskId: string, paneId: string) =>
    invoke<string>("request_pr_description", { taskId, paneId }),
  takePrDescription: (taskId: string) =>
    invoke<string | null>("take_pr_description", { taskId }),
  jiraSyncStatus: (key: string) => invoke<string | null>("jira_sync_status", { key }),
  jiraBrowse: (text: string, whose: string, includeDone: boolean, types: string[]) =>
    invoke<JiraPage>("jira_browse", { text: text || null, whose, includeDone, types }),
  jiraCreateFields: (projectKey: string, issueTypeId: string) =>
    invoke<CreateField[]>("jira_create_fields", { projectKey, issueTypeId }),
  jiraCreateIssue: (req: {
    summary: string;
    description: string;
    issue_type: string;
    project_key: string | null;
    parent_key: string | null;
    fields: Record<string, unknown> | null;
  }) => invoke<JiraIssue>("jira_create_issue", { req }),
  jiraCreateTask: (req: {
    summary: string;
    description: string;
    issue_type: string;
    project_key: string | null;
    parent_key: string | null;
    project_ids: string[];
    branch_suffix: string | null;
    base?: string | null;
  }) => invoke<Started>("jira_create_task", { req }),
  jiraStartWork: (
    key: string, projectIds: string[],
    agentId?: string | null, branchSuffix?: string | null, base?: string | null,
  ) => invoke<Started>("jira_start_work", {
    key, projectIds, agentId: agentId ?? null, branchSuffix: branchSuffix ?? null,
    base: base ?? null,
  }),

  // github
  githubConnect: (apiUrl: string, webUrl: string, token: string, reviewTeam: string | null) =>
    invoke<string>("github_connect", { apiUrl, webUrl, token, reviewTeam }),
  githubReviewQueue: () => invoke<ReviewQueue>("github_review_queue"),
  githubTaskPrs: (taskId: string) => invoke<CheckoutPr[]>("github_task_prs", { taskId }),
  githubAllPrs: () => invoke<TaskPrs[]>("github_all_prs"),
  setCheckoutBase: (checkoutId: string, base: string) =>
    invoke<void>("set_checkout_base", { checkoutId, base }),
  taskBranchFacts: (taskId: string) =>
    invoke<RepoBranchFacts[]>("task_branch_facts", { taskId }),
  checkoutBranches: (checkoutId: string) =>
    invoke<string[]>("checkout_branches", { checkoutId }),
  githubRetargetPr: (checkoutId: string) =>
    invoke<string>("github_retarget_pr", { checkoutId }),
  githubOpenPrs: (taskId: string, title: string, body: string, draft: boolean) =>
    invoke<RepoResult[]>("github_open_prs", { taskId, title, body, draft }),

  // slack
  slackConnect: (secret: string, channel: string) =>
    invoke<void>("slack_connect", { secret, channel }),
  slackNotify: (text: string, context?: string, kind?: string) =>
    invoke<boolean>("slack_notify", { text, context, kind: kind ?? null }),
  setSlackPrefs: (prefs: SlackConfig) => invoke<void>("set_slack_prefs", { prefs }),

  // settings
  getSettings: () => invoke<Settings>("get_settings"),
  takeNotices: () => invoke<AppNotice[]>("take_notices"),
  setWorktreeRoot: (path: string | null) => invoke<void>("set_worktree_root", { path }),
  setUiPrefs: (ui: UiPrefs) => invoke<void>("set_ui_prefs", { ui }),
  systemNotify: (title: string, body: string, view: "reviews" | "tickets") =>
    invoke<void>("system_notify", { title, body, view }),
  disconnect: (which: "jira" | "github" | "slack") => invoke<void>("disconnect", { which }),
  cursorIdeInstalled: () => invoke<boolean>("cursor_ide_installed"),
  openInCursor: (path: string) => invoke<void>("open_in_cursor", { path }),
};

export function errMessage(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return JSON.stringify(e);
}
