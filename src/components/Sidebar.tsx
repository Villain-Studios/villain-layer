import { Fragment, useEffect, useMemo, useState, type ReactNode } from "react";
import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
import { api, errMessage } from "../lib/api";
import { paneState, taskReview, taskTotals, useStore, type TaskReview } from "../store";
import type { JiraIssue, TaskView } from "../lib/types";
import { Combo, Confirm, ContextMenu, Field, Modal, Switch, type MenuItem } from "./ui";
import { RepoPicker } from "./RepoPicker";
import { read, write } from "../lib/persist";
import { IssueTypeIcon, typeMap } from "./IssueType";

/** What a task out for review is waiting on, in a word. */
const REVIEW_WORD: Partial<Record<TaskReview, { text: string; color: string }>> = {
  changes_requested: { text: "changes", color: "var(--red)" },
  approved: { text: "approved", color: "var(--green)" },
  merged: { text: "merged", color: "var(--green)" },
  commented: { text: "comments", color: "var(--blue)" },
};

export function Sidebar() {
  const { projects, tasks, panes, prs } = useStore();
  const settings = useStore((s) => s.settings);
  const issues = useStore((s) => s.issues);
  const issueTypes = useStore((s) => s.issueTypes);
  const refreshIssues = useStore((s) => s.refreshIssues);
  const selected = useStore((s) => s.selectedTask);
  const select = useStore((s) => s.select);
  const setView = useStore((s) => s.setView);
  const refreshTasks = useStore((s) => s.refreshTasks);
  const refreshPanes = useStore((s) => s.refreshPanes);
  const toast = useStore((s) => s.toast);
  const fail = useStore((s) => s.fail);

  const [creating, setCreating] = useState(false);
  const [name, setName] = useState("");
  const [branch, setBranch] = useState("");
  // Filing the ticket and opening the worktrees are the same act often enough
  // that they belong in one dialog rather than two views.
  const [withJira, setWithJira] = useState(false);
  const [jiraType, setJiraType] = useState("");
  const [jiraProject, setJiraProject] = useState(() => read("jiraProject", ""));
  const [jiraParent, setJiraParent] = useState("");
  const [jiraDesc, setJiraDesc] = useState("");
  const [picked, setPicked] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [addingRepoTo, setAddingRepoTo] = useState<TaskView | null>(null);
  const [adding, setAdding] = useState<string[]>([]);
  const [reason, setReason] = useState<string | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; task: TaskView } | null>(null);
  const [confirming, setConfirming] = useState<{
    title: string;
    body: ReactNode;
    label: string;
    run: () => void;
  } | null>(null);

  const tm = useMemo(() => typeMap(issueTypes), [issueTypes]);
  // Most sites never set a default project, so rather than disable the whole
  // feature the dialog asks — offering the keys already visible on the board.
  const boardKeys = useMemo(
    () => [...new Set(issues.map((i) => i.key.split("-")[0]).filter(Boolean))],
    [issues],
  );
  const defaultProject = settings?.jira?.project_key ?? boardKeys[0] ?? "";
  // A task is a normal issue: epics group work rather than being work, and a
  // sub-task needs a parent this dialog does not ask for.
  const creatable = useMemo(
    () => issueTypes.filter((t) => t.hierarchy_level === 0 && !t.subtask),
    [issueTypes],
  );
  // Epics from the board are what the dialog used to offer, which is only the
  // ones already carrying tickets. Jira is asked for the rest.
  const [projectEpics, setProjectEpics] = useState<JiraIssue[]>([]);
  const [epicsLoading, setEpicsLoading] = useState(false);
  const epicOptions = useMemo(
    () => projectEpics.map((e) => `${e.key} — ${e.summary}`),
    [projectEpics],
  );
  const epicLabel = jiraParent
    ? epicOptions.find((o) => o.startsWith(`${jiraParent} `)) ?? jiraParent
    : "";

  // Default to whatever the site calls a plain issue, without assuming it is
  // named "Task" — it often is not.
  useEffect(() => {
    if (jiraType && creatable.some((t) => t.name === jiraType)) return;
    const preferred = creatable.find((t) => t.name.toLowerCase() === "task") ?? creatable[0];
    setJiraType(preferred?.name ?? "");
  }, [creatable, jiraType]);

  // Asked for when the dialog is open and a project is named, and again when
  // that changes. Quiet on failure: the field still takes a typed key.
  useEffect(() => {
    if (!creating || !withJira || !jiraProject.trim()) { setProjectEpics([]); return; }
    let stop = false;
    setEpicsLoading(true);
    api.jiraEpics(jiraProject.trim())
      .then((list) => { if (!stop) setProjectEpics(list); })
      .catch(() => { if (!stop) setProjectEpics([]); })
      .finally(() => { if (!stop) setEpicsLoading(false); });
    return () => { stop = true; };
  }, [creating, withJira, jiraProject]);

  // Seeded when the dialog opens, and only then: refilling whenever the field
  // is empty would make it impossible to clear or retype.
  useEffect(() => {
    if (creating) setJiraProject((current) => current || defaultProject);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [creating]);

  useEffect(() => {
    if (!creating) { setReason(null); return; }
    api.suggestRepos({})
      .then((s) => { setPicked(s.project_ids); setReason(s.reason); })
      .catch(() => setPicked([]));
  }, [creating]);

  function closeCreate() {
    setCreating(false);
    setName("");
    setBranch("");
    setJiraDesc("");
    setJiraParent("");
  }

  async function createTask() {
    if (!name.trim() || picked.length === 0) return;
    setBusy(true);
    try {
      // With a ticket, the key it comes back with names the branch and the
      // folder, so the task cannot be built until Jira has answered.
      const task = withJira
        ? await api.jiraCreateTask({
            summary: name.trim(),
            description: jiraDesc.trim(),
            issue_type: jiraType,
            project_key: jiraProject.trim() || null,
            parent_key: jiraParent || null,
            project_ids: picked,
            branch_suffix: branch.trim() || null,
          })
        : await api.createTask({
            name: name.trim(),
            project_ids: picked,
            branch: branch.trim() || null,
          });
      await refreshTasks();
      if (withJira) {
        toast(
          "success",
          `Filed ${task.issue_key} and opened ${picked.length} worktree${
            picked.length === 1 ? "" : "s"
          }` + ("moved" in task && task.moved ? ` · ${task.moved}` : ""),
        );
        void refreshIssues();
      }
      select(task.id);
      closeCreate();
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  function askDeleteTask(task: TaskView) {
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
    try {
      const results = await api.deleteTask(task.id, force);
      const stuck = results.filter((r) => !r.ok);
      // Deleting a task stops its panes too, so the pane list is stale as well.
      await Promise.all([refreshTasks(), refreshPanes()]);

      if (stuck.length === 0) {
        if (selected === task.id) select(null);
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
    items.push(
      {
        label: "Copy branch name",
        onSelect: () => {
          navigator.clipboard
            .writeText(task.branch)
            .then(() => toast("success", `Copied ${task.branch}`))
            .catch(() => toast("error", "Could not reach the clipboard"));
        },
      },
      {
        label: "Reveal task folder",
        onSelect: () => void revealItemInDir(task.root).catch(fail),
      },
    );
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
  const fleetWaiting = fleet.some((p) => paneState(p).dot === "idle");

  // Two lists, because they are two questions. A task with a PR open is no
  // longer asking what to write — it is waiting on somebody, and what it is
  // waiting on is what the second list says.
  const reviewing = tasks.filter((t) =>
    (prs[t.id] ?? []).some((r) => r.pr && r.pr.state === "open"),
  );
  const working = tasks.filter((t) => !reviewing.includes(t));
  const ordered = [...working, ...reviewing];

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
            onClick={() => { setCreating(true); setName(""); setBranch(""); }}
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
          const waiting = mine.some((p) => paneState(p).dot === "idle");
          const totals = taskTotals(task);
          const multi = task.checkouts.length > 1;
          const isOpen = expanded[task.id] ?? false;

          const review = taskReview(prs[task.id] ?? []);
          const verdict = REVIEW_WORD[review];

          return (
            <Fragment key={task.id}>
              {i === working.length && reviewing.length > 0 && (
                <div className="section-head">
                  In review
                  <span className="count">{reviewing.length}</span>
                </div>
              )}
            <div>
              <div
                className={`ws${task.id === selected ? " active" : ""}`}
                onClick={() => select(task.id)}
                onContextMenu={(e) => {
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
                      setExpanded((x) => ({ ...x, [task.id]: !isOpen }));
                    }}
                  >
                    ▶
                  </span>
                  <span
                    className={`dot ${
                      totals.missing ? "gone" : waiting ? "idle" : live ? "live" : ""
                    }`}
                    title={waiting ? "An agent has gone quiet — it may need you" : undefined}
                  />
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
        <Modal
          title="New task"
          onClose={closeCreate}
          footer={
            <>
              <button className="btn" onClick={closeCreate}>Cancel</button>
              <button
                className="btn btn-primary"
                disabled={
                  busy || !name.trim() || picked.length === 0 ||
                  (withJira && (!jiraType || !jiraProject.trim()))
                }
                onClick={() => void createTask()}
              >
                {busy
                  ? withJira ? "Filing…" : "Creating…"
                  : withJira
                    ? `File ticket + ${picked.length} worktree${picked.length === 1 ? "" : "s"}`
                    : `Create ${picked.length} worktree${picked.length === 1 ? "" : "s"}`}
              </button>
            </>
          }
        >
          <Field label={withJira ? "Summary" : "Task name"}>
            <input
              autoFocus
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="fix checkout rounding"
            />
          </Field>

          <Switch
            label="File a Jira ticket for this"
            detail={
              settings?.jira_connected
                ? "Files the issue first, then names the branch and folder after its key."
                : "Connect Jira in Settings to file from here."
            }
            checked={withJira}
            disabled={!settings?.jira_connected}
            onChange={setWithJira}
          />

          {withJira && (
            <div className="jira-fields">
              <Field label="Project" hint="The key the ticket is filed under.">
                <Combo
                  value={jiraProject}
                  options={boardKeys}
                  placeholder="ACME"
                  width="100%"
                  onChange={(raw) => {
                    const v = raw.trim().toUpperCase();
                    setJiraProject(v);
                    write("jiraProject", v);
                  }}
                />
              </Field>

              <Field label="Type">
                <div className="type-row">
                  {creatable.map((t) => (
                    <button
                      key={t.id}
                      className={`type-pick${jiraType === t.name ? " active" : ""}`}
                      onClick={() => setJiraType(t.name)}
                    >
                      <IssueTypeIcon types={tm} name={t.name} size={15} />
                      {t.name}
                    </button>
                  ))}
                  {creatable.length === 0 && (
                    <span className="muted">Jira reported no issue types.</span>
                  )}
                </div>
              </Field>

              <Field
                label="Epic"
                hint={
                  epicsLoading
                    ? "Asking Jira for this project's epics…"
                    : "Optional. Every open epic on the project, whether or not it has work on it."
                }
              >
                <Combo
                  value={epicLabel}
                  options={epicOptions}
                  placeholder="No epic"
                  width="100%"
                  empty="No open epics on this project"
                  onChange={(v) => setJiraParent(v.trim().split(/\s/)[0] ?? "")}
                />
              </Field>

              <Field label="Description" hint="Optional. Becomes the ticket body and the agent's briefing.">
                <textarea
                  rows={4}
                  value={jiraDesc}
                  onChange={(e) => setJiraDesc(e.target.value)}
                  placeholder="What needs doing, and how you would know it is done."
                />
              </Field>
            </div>
          )}

          <Field
            label="Repositories"
            hint="One worktree per repo, all on the same branch, side by side in one task folder."
          >
            <RepoPicker
              projects={projects}
              picked={picked}
              onChange={setPicked}
              reason={reason}
            />
          </Field>

          <Field
            label={withJira ? "Branch suffix" : "Branch"}
            hint={
              withJira
                ? `Optional. The branch is the ticket key — ${jiraProject || "KEY"}-123, or ${jiraProject || "KEY"}-123-your-suffix.`
                : "Defaults to a slug of the task name. Used in every repo."
            }
          >
            <input
              value={branch}
              onChange={(e) => setBranch(e.target.value)}
              placeholder={withJira ? "(none)" : "(auto)"}
            />
          </Field>
        </Modal>
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
          onConfirm={confirming.run}
          onCancel={() => setConfirming(null)}
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
