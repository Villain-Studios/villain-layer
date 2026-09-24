/**
 * What the app's state means: pure functions over what the backend sent,
 * shared by the views, the store and the tests. Nothing here holds state or
 * reaches the backend.
 */
import type { CheckoutPr, JiraIssue, PaneInfo, Project, RepoHealth, TaskView, UpdateBy } from "./types";

/** Panes started from the Chat view carry this instead of a real task id. */
export const CHAT_TASK_ID = "chat";

/**
 * What a pane is doing, in a word and a colour.
 *
 * The backend works it out — from the agent's own hooks where it has them —
 * so this, the dock count and the banners all say the same thing. The amber
 * dot means it needs you: it is asking, or it finished and nobody has looked.
 */
export function paneState(pane: PaneInfo): { label: string; dot: string } {
  if (pane.running && pane.notice === "usage_limit") {
    return { label: "out of budget — hand off", dot: "gone" };
  }
  if (pane.running && pane.notice === "trust_prompt") {
    return { label: "waiting: trust this folder?", dot: "idle" };
  }
  if (!pane.running) {
    return {
      label: pane.exit_code === 0 ? "exited" : `exited with ${pane.exit_code ?? "?"}`,
      dot: pane.exit_code === 0 ? "" : "gone",
    };
  }
  if (pane.kind === "shell") return { label: "shell", dot: "live" };
  switch (pane.activity) {
    case "working": return { label: "working", dot: "live" };
    case "asking": return { label: "needs you — asking permission", dot: "idle" };
    // A chat that has answered is waiting as a chat does, not for you.
    case "done":
      return pane.task_id === CHAT_TASK_ID
        ? { label: "your turn", dot: "" }
        : { label: "finished — your turn", dot: "idle" };
    default: return { label: "idle", dot: "" };
  }
}

/**
 * An agent that needs you — what the dock icon counts.
 *
 * The same rule as the backend's: asking, or finished and not yet seen. A
 * chat that has finished is not news — sitting at its prompt is what it is
 * for — but one asking permission is as stuck as any other.
 */
export function needsYou(pane: PaneInfo): boolean {
  return (
    pane.kind === "agent" &&
    pane.running &&
    (pane.activity === "asking" || (pane.activity === "done" && pane.task_id !== CHAT_TASK_ID))
  );
}

/** A running agent — what every "N running" in the app counts. */
export function isRunningAgent(pane: PaneInfo): boolean {
  return pane.kind === "agent" && pane.running;
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

/**
 * What is wrong with a repository, in a sentence, or null: a clone that is
 * gone or has become another repository, and task worktrees git cannot
 * read. All of it was found by hand once, before the Repos view said so.
 */
export function repoTrouble(p: Project, h: RepoHealth | undefined, tasks: TaskView[]): string | null {
  if (h?.clone === "missing") return `The clone is not at ${p.path} any more.`;
  if (h?.clone === "not_repo") return `${p.path} is not a git repository any more.`;
  if (h?.clone === "other") {
    return `${p.path} is a different repository now, not the one the app's copy fetches from (${h.origin ?? "unknown"}).`;
  }
  const unlinked = tasks.flatMap((t) => t.checkouts).filter((c) => c.project_id === p.id && c.broken).length;
  if (unlinked > 0) return `${unlinked} task worktree${unlinked === 1 ? "" : "s"} git cannot read.`;
  return null;
}

/**
 * How Update from base updates branches in a repository, and why (UPD-7):
 * what it is set to, else what its history suggests, else merge, which
 * rewrites nothing that was pushed.
 */
export function updateByFor(p: Project | undefined, h: RepoHealth | undefined): { by: UpdateBy; why: string } {
  if (p?.update_by) return { by: p.update_by, why: "Chosen for this repo, in Repos or at its last update." };
  if (h?.update_guess) return { by: h.update_guess, why: `Guessed: ${h.update_reason ?? "from its history"}.` };
  return { by: "merge", why: "Nothing to go on yet. A merge rewrites nothing that was pushed." };
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
      unlinked: acc.unlinked + (c.broken ? 1 : 0),
      changed: acc.changed + c.changed,
    }),
    { dirty: 0, staged: 0, ahead: 0, behind: 0, conflicted: 0, missing: 0, unlinked: 0, changed: 0 },
  );
}

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
  // Verdicts only from the open ones. A merged PR's reviews are not fetched,
  // so its verdict is "none" — and asking every live row to be approved made
  // "approved" unreachable for a task with any repo merged.
  const open = live.filter((r) => r.pr!.state === "open");
  if (open.some((r) => r.verdict === "changes_requested")) return "changes_requested";
  // For a merged repo, `changed` counts from what it landed, so this is work
  // committed after the merge.
  if (rows.some((r) => (!r.pr || r.pr.state !== "open") && r.changed > 0)) return "incomplete";
  if (open.every((r) => r.verdict === "approved")) return "approved";
  if (open.some((r) => r.verdict === "commented")) return "commented";
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

/**
 * What the "Branch from" box should hold after the picked repos change:
 * their shared default branch, or blank ("each repo's own default") when
 * they disagree. A base typed by hand stays; a box still showing the last
 * suggestion follows the new one.
 *
 * `last` is the suggestion made before this one. It is passed in, not read
 * from a ref inside a state updater: the updater ran after the ref had
 * already moved on, so the old suggestion looked typed and stuck. Pick
 * customer-portal (base `dev`), swap it for two repos on `development`, and
 * Start work cut both from `dev`: "fatal: invalid reference: dev".
 */
export function suggestBase(current: string, last: string, defaults: readonly string[]): string {
  const unique = [...new Set(defaults)];
  const suggested = unique.length === 1 ? unique[0] : "";
  return current === "" || current === last ? suggested : current;
}
