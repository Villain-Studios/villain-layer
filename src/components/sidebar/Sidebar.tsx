import { Fragment, useState, type ReactNode } from "react";
import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
import { api, errMessage } from "../../lib/api";
import { copyText } from "../../lib/clipboard";
import { paneState, taskReview, taskTotals, useNow, useStore, type TaskReview } from "../../store";
import type { TaskView } from "../../lib/types";
import { BusyOverlay, Confirm, ContextMenu, Field, Modal, Spinner, type MenuItem } from "../ui";
import { RepoPicker } from "../RepoPicker";
import { CreateTaskDialog } from "./CreateTaskDialog";

/** What a task out for review is waiting on, in a word. */
const REVIEW_WORD: Partial<Record<TaskReview, { text: string; color: string }>> = {
  changes_requested: { text: "changes", color: "var(--red)" },
  approved: { text: "approved", color: "var(--green)" },
  merged: { text: "merged", color: "var(--green)" },
  commented: { text: "comments", color: "var(--blue)" },
};

export function Sidebar() {
  const projects = useStore((s) => s.projects);
  const tasks = useStore((s) => s.tasks);
  const panes = useStore((s) => s.panes);
  const prs = useStore((s) => s.prs);
  const settings = useStore((s) => s.settings);
  const issues = useStore((s) => s.issues);
  const issueTypes = useStore((s) => s.issueTypes);
  const selected = useStore((s) => s.selectedTask);
  const select = useStore((s) => s.select);
  const setView = useStore((s) => s.setView);
  const showIssue = useStore((s) => s.showIssue);
  const refreshTasks = useStore((s) => s.refreshTasks);
  const refreshPanes = useStore((s) => s.refreshPanes);
  const toast = useStore((s) => s.toast);
  const fail = useStore((s) => s.fail);
  const cursorIde = useStore((s) => s.cursorIde);
  const now = useNow(10_000);

  const [creating, setCreating] = useState(false);
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [addingRepoTo, setAddingRepoTo] = useState<TaskView | null>(null);
  const [adding, setAdding] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [menu, setMenu] = useState<{ x: number; y: number; task: TaskView } | null>(null);
  const [confirming, setConfirming] = useState<{
    title: string;
    body: ReactNode;
    label: string;
    /** A `run` that returns a promise keeps the dialog up and spinning. */
    busyLabel?: string;
    run: () => void | Promise<unknown>;
  } | null>(null);
  const [deleting, setDeleting] = useState<{ id: string; name: string } | null>(null);

  function askDeleteTask(task: TaskView) {
    if (deleting) return;
    const { dirty } = taskTotals(task);
    const repos = task.checkouts.length;
    setConfirming({
      title: "Delete task",
      label: "Delete task",
      body: (
        <>
          Delete <b>{task.name}</b> and remove {repos} worktree
          {repos === 1 ? "" : "s"} from disk?
          <div className="muted" style={{ marginTop: 8 }}>
            Branch <code>{task.branch}</code> is left alone, in the repositories and on
            any remote.
          </div>
          {dirty > 0 && (
            <div className="confirm-detail">
              {dirty} uncommitted change{dirty === 1 ? "" : "s"} will be lost. This cannot
              be undone.
            </div>
          )}
        </>
      ),
      run: () => void runDelete(task, dirty > 0),
    });
  }

  /// Add every repo picked, reporting per repo rather than stopping at the
  /// first failure: one worktree that will not create is no reason to skip the
  /// rest, and the user needs to know which one it was.
  async function addRepos(task: TaskView) {
    setBusy(true);
    const failed: string[] = [];
    let told = 0;
    for (const id of adding) {
      try {
        told = Math.max(told, (await api.addCheckout(task.id, id)).told);
      } catch (e) {
        failed.push(`${projects.find((p) => p.id === id)?.name ?? id}: ${errMessage(e)}`);
      }
    }
    await Promise.all([refreshTasks(), refreshPanes()]);
    setBusy(false);
    setAddingRepoTo(null);
    setAdding([]);
    setExpanded((e) => ({ ...e, [task.id]: true }));

    const added = adding.length - failed.length;
    if (added > 0) {
      toast(
        "success",
        `${added} worktree${added === 1 ? "" : "s"} on ${task.branch}` +
          // Agents already running cannot see a new sibling folder, so they are
          // told. Say so rather than reaching into their session invisibly.
          (told > 0 ? ` · told ${told} running agent${told === 1 ? "" : "s"}` : ""),
      );
    }
    if (failed.length) toast("error", failed.join("\n"));
  }

  /// git refuses to remove a worktree with uncommitted or untracked files. Say
  /// so and offer to force, rather than leaving an orphan nobody can see.
  async function runDelete(task: TaskView, force: boolean) {
    setConfirming(null);
    setDeleting({ id: task.id, name: task.name });
    try {
      const results = await api.deleteTask(task.id, force);
      const stuck = results.filter((r) => !r.ok);
      // Deleting a task stops its panes too, so the pane list is stale as well.
      await Promise.all([refreshTasks(), refreshPanes()]);

      if (stuck.length === 0) {
        if (selected === task.id) select(null);
        toast("success", `Deleted ${task.name}`);
        return;
      }
      setConfirming({
        title: "Some worktrees could not be removed",
        label: "Force delete",
        body: (
          <>
            git refused to remove {stuck.length} worktree{stuck.length === 1 ? "" : "s"},
            so the task has been kept rather than leaving them orphaned:
            <div className="confirm-detail">
              {stuck.map((r) => (
                <div key={r.checkout_id}>
                  <b>{r.repo}</b> — {r.detail}
                </div>
              ))}
            </div>
            <div style={{ marginTop: 10 }}>
              Forcing deletes those worktrees and anything uncommitted in them.
            </div>
          </>
        ),
        run: () => void runDelete(task, true),
      });
    } catch (e) {
      fail(e);
    } finally {
      setDeleting(null);
    }
  }

  function askRemoveRepo(checkoutId: string, repoName: string, dirty: number, last: boolean) {
    setConfirming({
      title: "Remove repository from task",
      label: "Remove",
      body: (
        <>
          Remove <b>{repoName}</b> from this task and delete its worktree?
          <div className="muted" style={{ marginTop: 8 }}>
            Any terminal running inside it is stopped. Panes started at the task
            root, which see every repo, are left alone.
          </div>
          {last && (
            <div className="confirm-detail">
              This is the only repository in the task. Removing it leaves nothing to
              work in — add another, or delete the task instead.
            </div>
          )}
          {dirty > 0 && (
            <div className="confirm-detail">
              {dirty} uncommitted change{dirty === 1 ? "" : "s"} in that worktree will be
              lost.
            </div>
          )}
        </>
      ),
      // Awaited by the dialog, so the spinner is up for the whole thing —
      // stopping the panes alone waits two seconds on them, and `git worktree
      // remove` is as slow here as it is when a whole task goes.
      busyLabel: "Removing…",
      run: async () => {
        try {
          await api.removeCheckout(checkoutId, dirty > 0);
          // Panes rooted in that worktree are stopped with it, so refresh both
          // or the terminals stay on screen until the next poll happens to run.
          await Promise.all([refreshTasks(), refreshPanes()]);
        } catch (e) {
          fail(e);
        }
      },
    });
  }

  function taskMenu(task: TaskView): MenuItem[] {
    const items: MenuItem[] = [
      { label: "Open", onSelect: () => select(task.id) },
    ];
    if (task.issue_url) {
      items.push({
        label: `Open ${task.issue_key} in Jira`,
        onSelect: () => void openUrl(task.issue_url!),
      });
    }
    if (task.issue_key) {
      items.push({
        label: "Show in Tickets",
        onSelect: () => showIssue(task.issue_key!),
      });
    }
    items.push(
      {
        label: "Copy branch name",
        onSelect: () => {
          void copyText(task.branch)
            .then(() => toast("success", `Copied ${task.branch}`))
            .catch(() => toast("error", "Could not reach the clipboard"));
        },
      },
      {
        label: "Reveal task folder",
        onSelect: () => void revealItemInDir(task.root).catch(fail),
      },
    );
    if (cursorIde) {
      items.push({
        label: "Open in Cursor",
        onSelect: () => void api.openInCursor(task.root).catch(fail),
      });
    }
    if (available(task).length > 0) {
      items.push({
        label: "Add a repository…",
        separated: true,
        onSelect: () => setAddingRepoTo(task),
      });
    }
    items.push({
      label: "Delete task…",
      danger: true,
      separated: available(task).length === 0,
      onSelect: () => askDeleteTask(task),
    });
    return items;
  }

  const available = (task: TaskView) =>
    projects.filter((p) => !task.checkouts.some((c) => c.project_id === p.id));

  // The same panes AgentsView counts, so the row and the view it opens agree.
  const fleet = panes.filter((p) => p.kind === "agent" && p.running);
  const fleetWaiting = fleet.some((p) => paneState(p, now).dot === "idle");

  // Three lists, because they are three questions. Writing, waiting on a
  // reviewer, and landed (update the ticket) are not the same job — and a
  // merged task in the first list looks like unfinished work.
  const done = tasks.filter((t) => taskReview(prs[t.id] ?? []) === "merged");
  const reviewing = tasks.filter(
    (t) =>
      !done.includes(t) &&
      (prs[t.id] ?? []).some((r) => r.pr && r.pr.state === "open"),
  );
  const working = tasks.filter((t) => !done.includes(t) && !reviewing.includes(t));
  const ordered = [...working, ...reviewing, ...done];

  return (
    <div className="sidebar">
      <div className="sidebar-body" style={{ paddingTop: 6 }}>
        {/*
          The overview is a place, not merely what you see before choosing.
          Selecting a task is remembered across restarts, so without a way
          back the first task ever clicked is the last view the app offers.
        */}
        <div
          className={`ws${selected === null ? " active" : ""}`}
          onClick={() => select(null)}
        >
          <div className="ws-title">
            <span
              className={`dot ${fleetWaiting ? "idle" : fleet.length ? "live" : ""}`}
              title={fleetWaiting ? "An agent has gone quiet — it may need you" : undefined}
            />
            <span className="label">All agents</span>
          </div>
          <div className="ws-meta">
            <span>across every task</span>
            <div className="spacer" />
            {fleet.length > 0 && (
              <span style={fleetWaiting ? { color: "var(--amber)" } : undefined}>
                {fleet.length}▶
              </span>
            )}
          </div>
        </div>

        <div className="section-head">
          Tasks
          <span className="count">{working.length}</span>
          <div className="spacer" />
          <button
            className="btn-sm"
            title="New task"
            disabled={projects.length === 0}
            onClick={() => setCreating(true)}
          >
            +
          </button>
        </div>

        {tasks.length === 0 && (
          <div className="muted" style={{ padding: "4px 8px 10px", lineHeight: 1.5 }}>
            {projects.length === 0 ? (
              <>
                No repositories yet —{" "}
                <a
                  href="#"
                  style={{ color: "var(--accent)" }}
                  onClick={(e) => { e.preventDefault(); setView("repos"); }}
                >
                  add some in the Repos tab
                </a>
                .
              </>
            ) : (
              "No tasks yet. Create one, or start from a Jira ticket."
            )}
          </div>
        )}

        {ordered.map((task, i) => {
          const mine = panes.filter((p) => p.task_id === task.id && p.running);
          const live = mine.length;
          // Surface "waiting on you" here too, not just in the overview.
          const waiting = mine.some((p) => paneState(p, now).dot === "idle");
          const totals = taskTotals(task);
          const multi = task.checkouts.length > 1;
          const isOpen = expanded[task.id] ?? false;

          const review = taskReview(prs[task.id] ?? []);
          const verdict = REVIEW_WORD[review];
          const vanishing = deleting?.id === task.id;

          return (
            <Fragment key={task.id}>
              {i === working.length && reviewing.length > 0 && (
                <div className="section-head">
                  In review
                  <span className="count">{reviewing.length}</span>
                </div>
              )}
              {i === working.length + reviewing.length && done.length > 0 && (
                <div className="section-head">
                  Done
                  <span className="count">{done.length}</span>
                </div>
              )}
            <div>
              <div
                className={`ws${task.id === selected ? " active" : ""}${vanishing ? " deleting" : ""}`}
                onClick={() => { if (!vanishing) select(task.id); }}
                onContextMenu={(e) => {
                  if (vanishing) return;
                  e.preventDefault();
                  setMenu({ x: e.clientX, y: e.clientY, task });
                }}
              >
                <div className="ws-title">
                  {/*
                    Every task expands, whatever its repo count. Gating this on
                    having several meant a task with one repo could not show or
                    remove it — and a task that dropped to one lost its rows
                    while keeping the "add repo" beneath them, with no way left
                    to close it.
                  */}
                  <span
                    className={`chev${isOpen ? " open" : ""}`}
                    onClick={(e) => {
                      e.stopPropagation();
                      if (vanishing) return;
                      setExpanded((x) => ({ ...x, [task.id]: !isOpen }));
                    }}
                  >
                    ▶
                  </span>
                  {vanishing ? (
                    <Spinner />
                  ) : (
                  <span
                    className={`dot ${
                      totals.missing ? "gone" : waiting ? "idle" : live ? "live" : ""
                    }`}
                    title={waiting ? "An agent has gone quiet — it may need you" : undefined}
                  />
                  )}
                  {task.issue_key && <span className="key-chip">{task.issue_key}</span>}
                  <span className="label">
                    {task.issue_key ? task.name.replace(`${task.issue_key} `, "") : task.name}
                  </span>
                </div>
                <div className="ws-meta">
                  <span style={{ overflow: "hidden", textOverflow: "ellipsis" }}>
                    {task.branch}
                  </span>
                  <div className="spacer" />
                  {multi && <span>{task.checkouts.length} repos</span>}
                  {live > 0 && (
                    <span style={waiting ? { color: "var(--amber)" } : undefined}>
                      {live}▶
                    </span>
                  )}
                  {totals.dirty > 0 && (
                    <span style={{ color: "var(--amber)" }}>±{totals.dirty}</span>
                  )}
                  {verdict && <span style={{ color: verdict.color }}>{verdict.text}</span>}
                </div>
              </div>

              {isOpen && task.checkouts.map((c) => {
                const d = c.status ? c.status.unstaged + c.status.untracked : 0;
                return (
                  <div key={c.id} className="repo-row">
                    <span className={`dot ${c.exists ? "" : "gone"}`} />
                    <span className="rname">{c.project_name}</span>
                    {d > 0 && <span style={{ color: "var(--amber)" }}>±{d}</span>}
                    <span
                      className="x"
                      title="Remove repo from task"
                      onClick={(e) => {
                        e.stopPropagation();
                        askRemoveRepo(c.id, c.project_name, d, task.checkouts.length === 1);
                      }}
                    >
                      ✕
                    </span>
                  </div>
                );
              })}

              {isOpen && available(task).length > 0 && (
                <div
                  className="repo-row"
                  style={{ color: "var(--accent)", cursor: "pointer" }}
                  onClick={() => setAddingRepoTo(task)}
                >
                  <span className="rname">+ add repo</span>
                </div>
              )}
            </div>
            </Fragment>
          );
        })}

      </div>

      {creating && (
        <CreateTaskDialog
          projects={projects}
          settings={settings}
          issues={issues}
          issueTypes={issueTypes}
          onClose={() => setCreating(false)}
        />
      )}

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          onClose={() => setMenu(null)}
          items={taskMenu(menu.task)}
        />
      )}

      {confirming && (
        <Confirm
          title={confirming.title}
          body={confirming.body}
          confirmLabel={confirming.label}
          busyLabel={confirming.busyLabel}
          onConfirm={confirming.run}
          onCancel={() => setConfirming(null)}
        />
      )}

      {deleting && (
        <BusyOverlay
          title={`Deleting ${deleting.name}`}
          detail="Stopping agents and removing worktrees…"
        />
      )}

      {addingRepoTo && (
        <Modal
          title={`Add repositories to ${addingRepoTo.issue_key ?? addingRepoTo.branch}`}
          onClose={() => { setAddingRepoTo(null); setAdding([]); }}
          footer={
            <>
              <button
                className="btn"
                onClick={() => { setAddingRepoTo(null); setAdding([]); }}
              >
                Cancel
              </button>
              <button
                className="btn btn-primary"
                disabled={busy || adding.length === 0}
                onClick={() => void addRepos(addingRepoTo)}
              >
                {busy
                  ? "Creating…"
                  : `Add ${adding.length} repo${adding.length === 1 ? "" : "s"}`}
              </button>
            </>
          }
        >
          <Field
            label="Repositories"
            hint={`A worktree on ${addingRepoTo.branch} is created for each, beside the ones already in this task.`}
          >
            <RepoPicker
              projects={available(addingRepoTo)}
              picked={adding}
              onChange={setAdding}
            />
          </Field>
        </Modal>
      )}
    </div>
  );
}
