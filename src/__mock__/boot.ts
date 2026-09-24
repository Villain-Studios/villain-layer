/**
 * The real UI in a plain browser, with the Rust side answered from `world.ts`.
 *
 * The native window can be screenshotted but not driven, so this is how a
 * change to the UI gets looked at and clicked through — by a person or an
 * agent with a browser. Served by the same Vite that `bun run dev` and
 * `bun run dev:app` start; open /mock.html on it. Nothing here is bundled
 * into the app: `vite build` only builds index.html.
 *
 * URL parameters set the scene before the app boots:
 *
 *   ?scenario=busy|empty|unlinked|tickets   which world (default busy)
 *   &view=work|tickets|reviews|chat|repos
 *   &task=t-login          the selected task (none: the All agents overview)
 *   &tab=terminals|diff|pr
 *
 * From the console, or a browser tool's JavaScript:
 *
 *   __mock.calls                     every command invoked, with its arguments
 *   __mock.on("cmd", (args) => …)    answer a command differently from now on
 *   __mock.emit("pty:activity", id)  deliver a backend event
 *   __mock.world                     the data being served; edit it, then emit
 *
 * A command with no answer here logs "[mock] unanswered" and returns null.
 * That is the cue to add it to `answer` below.
 */
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { emit } from "@tauri-apps/api/event";
import type { Catchup, Cleaned, FlowStatus, PaneInfo, Project, RepoUpdate, Synced } from "../lib/types";
import { ago, SCENARIOS } from "./world";

type Args = Record<string, unknown>;
type Answer = (args: Args) => unknown;

const params = new URLSearchParams(location.search);
// A Map, so only the scenarios' own names are found: looked up on the plain
// object, `?scenario=toString` found Object's method and built no world.
const scenarios = new Map(Object.entries(SCENARIOS));
const world = (scenarios.get(params.get("scenario") ?? "busy") ?? SCENARIOS.busy)();
const calls: { cmd: string; args: Args }[] = [];
const overrides = new Map<string, Answer>();

/** Base64 of the UTF-8 bytes, as the backend sends it, and how many bytes that was. */
function encode(text: string): { data: string; end: number } {
  const bytes = new TextEncoder().encode(text);
  return { data: btoa(String.fromCharCode(...bytes)), end: bytes.length };
}

function newPane(args: Args, kind: PaneInfo["kind"], task: string): PaneInfo {
  const agent = world.agents.find((a) => a.id === args.agentId);
  const p: PaneInfo = {
    id: `pane-${Date.now()}`,
    task_id: task,
    checkout_id: (args.checkoutId as string | null) ?? null,
    kind,
    title: agent?.name ?? "Shell",
    agent_id: agent?.id ?? null,
    cwd: world.tasks.find((t) => t.id === task)?.root ?? "/Users/you",
    running: true,
    exit_code: null,
    started_at: ago(0),
    last_output_at: ago(0),
    notice: null,
    activity: kind === "agent" ? "working" : "idle",
    activity_since: ago(0),
  };
  world.panes.push(p);
  world.output[p.id] = kind === "agent" ? `${p.title} starting…\r\n` : "you@mac % ";
  return p;
}

const answer: Record<string, Answer> = {
  // What the app asks for on every launch.
  // A copy, as the real IPC's JSON always is: handing back the object a
  // setter just changed looked like no change to the store, and nothing redrew.
  get_settings: () => structuredClone(world.settings),
  take_notices: () => [],
  list_messages: () => structuredClone(world.messages),
  mark_messages_read: (a) => {
    const ids = a.ids as number[] | null;
    for (const m of world.messages) if (!ids || ids.includes(m.id)) m.read = true;
    void emit("messages:changed");
    return null;
  },
  clear_messages: () => {
    world.messages = [];
    void emit("messages:changed");
    return null;
  },
  cursor_ide_installed: () => false,
  list_projects: () => world.projects,
  list_tasks: () => world.tasks,
  list_panes: () => world.panes,
  list_agents: () => world.agents,

  // Terminals.
  pty_attach: (a): Catchup => {
    return { ...encode(world.output[a.paneId as string] ?? ""), reset: false };
  },
  // Keys, sizes and visibility: nothing to answer, and too frequent to log.
  pty_write: () => null,
  pty_resize: () => null,
  pty_detach: () => null,
  resumable_agents: () => [],
  task_prompt: () => "Work on ACME-123: Fix login race.\n\nThe ticket says…",
  spawn_agent: (a) => newPane(a, "agent", a.taskId as string),
  spawn_shell: (a) => newPane(a, "shell", a.taskId as string),
  spawn_chat: (a) => newPane(a, "agent", "chat"),
  close_pane: (a) => {
    world.panes = world.panes.filter((p) => p.id !== a.paneId);
    return null;
  },
  kill_pane: (a) => {
    const p = world.panes.find((x) => x.id === a.paneId);
    if (p) Object.assign(p, { running: false, exit_code: 143 });
    return null;
  },

  // Diff.
  diff_files: () => world.changed,
  diff_file: (a) =>
    `--- a/${a.path}\n+++ b/${a.path}\n@@ -1,3 +1,4 @@\n const retries = config.retries;\n-let attempt = 1;\n+let attempt = 0;\n+// Counted from zero: the first try is not a retry.\n while (attempt < retries) {\n`,
  task_commits: () => world.tasks.flatMap((t) =>
    t.checkouts.map((c) => ({ checkout_id: c.id, repo: c.project_name, commits: [] }))),
  task_branch_facts: () => [],

  // GitHub.
  github_all_prs: () => world.prs,
  github_task_prs: (a) => world.prs.find((p) => p.task_id === a.taskId)?.rows ?? [],
  github_review_queue: () => world.reviews,
  github_pr_feedback: () => world.feedback,
  checkout_branches: () => ["main", "develop"],

  // Jira.
  jira_issues: () => ({ issues: world.issues, more: false }),
  jira_issue_types: () => world.issueTypes,
  jira_epics: () => [],
  jira_transitions: () => world.transitions,
  jira_transition: (a) => {
    const t = world.transitions.find((x) => x.id === a.transitionId);
    const i = world.issues.find((x) => x.key === a.key);
    if (t && i) Object.assign(i, { status: t.to_status, status_id: t.to_id, status_category: t.to_category });
    return null;
  },
  jira_project_statuses: () => [
    { id: "1", name: "To Do", category: "new" },
    { id: "3", name: "In Progress", category: "indeterminate" },
    { id: "10322", name: "Review", category: "indeterminate" },
    { id: "10323", name: "Testing", category: "indeterminate" },
    { id: "10002", name: "Done", category: "done" },
    { id: "10016", name: "Cancelled", category: "done" },
  ],
  set_ticket_flow: (a) => {
    const jira = world.settings.jira;
    if (!jira) throw "Jira is not configured";
    const flow = (jira.flow[a.projectKey as string] ??= { review: null, merged: null });
    flow[a.stage as "review" | "merged"] = (a.status as FlowStatus | null) ?? null;
    return null;
  },
  delete_task: (a) => {
    world.tasks = world.tasks.filter((t) => t.id !== a.id);
    return [];
  },
  jira_create_fields: () => world.requiredFields,
  jira_browse: () => ({ issues: world.issues, more: false }),
  suggest_repos: () => ({ project_ids: [], reason: null }),
  project_branches: () => ["main", "develop"],

  // Repos: health, Sync, Locate, Clean up.
  repo_health: () => world.health,
  sync_repos: (a) =>
    (a.projectIds as string[]).map((id): Synced => {
      const p = world.projects.find((x) => x.id === id);
      const h = world.health.find((x) => x.project_id === id);
      if (!p || !h) return { project_id: id, repo: id, ok: false, detail: "No such repository" };
      if (h.clone !== "ok") {
        return { project_id: id, repo: p.name, ok: true, detail: `Fetched. The clone is not at ${p.path} any more: Locate it.` };
      }
      const moved = h.ahead ? 0 : h.behind ?? 0;
      h.synced_at = Math.round(Date.now() / 1000);
      if (moved) h.behind = 0;
      const detail = h.ahead
        ? `Fetched. main has ${h.ahead} commit of its own, so it was left alone.`
        : moved ? `Fetched. main moved forward ${moved} commits.` : "Fetched. main is up to date.";
      return { project_id: id, repo: p.name, ok: true, detail };
    }),
  locate_project: (a) => {
    const p = world.projects.find((x) => x.id === a.projectId);
    const h = world.health.find((x) => x.project_id === a.projectId);
    if (!p || !h) throw "no such repository";
    p.path = a.path as string;
    Object.assign(h, { clone: "ok", found: null, origin: `git@github.com:acme/${p.name}.git` });
    return p;
  },
  set_project_update_by: (a) => {
    const p = world.projects.find((x) => x.id === a.projectId);
    if (p) p.update_by = (a.by as Project["update_by"]) ?? null;
    return null;
  },
  update_from_base: (a) =>
    (a.picks as { checkout_id: string; by: string }[]).map((pick): RepoUpdate => {
      const c = world.tasks.flatMap((t) => t.checkouts).find((x) => x.id === pick.checkout_id);
      return {
        checkout_id: pick.checkout_id, repo: c?.project_name ?? "?", base: c?.base ?? "main",
        outcome: "updated", commits: 2, conflicts: [],
        detail: pick.by === "rebase" ? "rebased onto 2 new commits" : "merged 2 commits",
      };
    }),
  cleanup_plan: () => world.cleanup,
  cleanup_apply: (a) => {
    const ids = a.ids as string[];
    const done: Cleaned[] = ids.map((id) => {
      const item = world.cleanup.find((i) => i.id === id);
      if (!item) return { id, ok: false, detail: "Changed since the list was made. Look again." };
      return item.verdict === "blocked" ? { id, ok: false, detail: item.detail } : { id, ok: true, detail: "" };
    });
    world.cleanup = world.cleanup.filter((i) => !done.some((d) => d.ok && d.id === i.id));
    return done;
  },

  // Plugins the UI reaches through @tauri-apps packages.
  "plugin:notification|is_permission_granted": () => true,
  "plugin:app|version": () => "0.0.0-mock",
  "plugin:window|is_focused": () => true,
};

// Before anything imports @tauri-apps/api/window, which reads these.
mockWindows("main");
mockIPC(
  (cmd, args) => {
    const a = (args ?? {}) as Args;
    calls.push({ cmd, args: a });
    const fn = overrides.get(cmd) ?? answer[cmd];
    if (fn) return fn(a);
    if (!cmd.startsWith("plugin:")) console.warn("[mock] unanswered", cmd, a);
    return null;
  },
  { shouldMockEvents: true },
);

(window as unknown as { __mock: unknown }).__mock = {
  calls,
  world,
  on: (cmd: string, fn: Answer) => overrides.set(cmd, fn),
  emit: (event: string, payload?: unknown) => emit(event, payload),
};

// The store reads its layout from here on boot (lib/persist.ts).
const scene: Record<string, string | null> = {
  view: params.get("view") ?? "work",
  selectedTask: params.get("task"),
  tab: params.get("tab") ?? "terminals",
};
for (const [key, value] of Object.entries(scene)) {
  localStorage.setItem(`villain.${key}`, JSON.stringify(value));
}

await import("../main");
