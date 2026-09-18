import { useEffect, useState } from "react";
import { api } from "../lib/api";
import { paneState, taskTotals, useStore } from "../store";
import type { TaskView } from "../lib/types";
import { Field, Modal } from "./ui";
import { RepoPicker } from "./RepoPicker";

export function Sidebar() {
  const { projects, tasks, panes } = useStore();
  const selected = useStore((s) => s.selectedTask);
  const select = useStore((s) => s.select);
  const setView = useStore((s) => s.setView);
  const refreshTasks = useStore((s) => s.refreshTasks);
  const fail = useStore((s) => s.fail);

  const [creating, setCreating] = useState(false);
  const [name, setName] = useState("");
  const [branch, setBranch] = useState("");
  const [picked, setPicked] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [addingRepoTo, setAddingRepoTo] = useState<TaskView | null>(null);
  const [reason, setReason] = useState<string | null>(null);

  useEffect(() => {
    if (!creating) { setReason(null); return; }
    api.suggestRepos({})
      .then((s) => { setPicked(s.project_ids); setReason(s.reason); })
      .catch(() => setPicked([]));
  }, [creating]);

  async function createTask() {
    if (!name.trim() || picked.length === 0) return;
    setBusy(true);
    try {
      const task = await api.createTask({
        name: name.trim(),
        project_ids: picked,
        branch: branch.trim() || null,
      });
      await refreshTasks();
      select(task.id);
      setCreating(false);
      setName("");
      setBranch("");
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  async function deleteTask(task: TaskView) {
    const { dirty } = taskTotals(task);
    const warning = dirty > 0 ? `\n\n${dirty} uncommitted change(s) will be lost.` : "";
    const repos = task.checkouts.length;
    if (!confirm(`Delete "${task.name}" and its ${repos} worktree(s)?${warning}`)) return;
    try {
      await api.deleteTask(task.id, dirty > 0);
      if (selected === task.id) select(null);
      await refreshTasks();
    } catch (e) {
      fail(e);
    }
  }

  async function addRepo(task: TaskView, projectId: string) {
    try {
      await api.addCheckout(task.id, projectId);
      await refreshTasks();
      setAddingRepoTo(null);
      setExpanded((e) => ({ ...e, [task.id]: true }));
    } catch (e) {
      fail(e);
    }
  }

  async function removeRepo(checkoutId: string, repoName: string, dirty: number) {
    const warning = dirty > 0 ? `\n\n${dirty} uncommitted change(s) will be lost.` : "";
    if (!confirm(`Remove ${repoName} from this task?${warning}`)) return;
    try {
      await api.removeCheckout(checkoutId, dirty > 0);
      await refreshTasks();
    } catch (e) {
      fail(e);
    }
  }


  const available = (task: TaskView) =>
    projects.filter((p) => !task.checkouts.some((c) => c.project_id === p.id));

  return (
    <div className="sidebar">
      <div className="sidebar-body" style={{ paddingTop: 6 }}>
        <div className="section-head">
          Tasks
          <span className="count">{tasks.length}</span>
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

        {tasks.map((task) => {
          const mine = panes.filter((p) => p.task_id === task.id && p.running);
          const live = mine.length;
          // Surface "waiting on you" here too, not just in the overview.
          const waiting = mine.some((p) => paneState(p).dot === "idle");
          const totals = taskTotals(task);
          const multi = task.checkouts.length > 1;
          const isOpen = expanded[task.id] ?? false;

          return (
            <div key={task.id}>
              <div
                className={`ws${task.id === selected ? " active" : ""}`}
                onClick={() => select(task.id)}
                onContextMenu={(e) => { e.preventDefault(); void deleteTask(task); }}
                title="Right-click to delete"
              >
                <div className="ws-title">
                  {multi ? (
                    <span
                      className={`chev${isOpen ? " open" : ""}`}
                      onClick={(e) => {
                        e.stopPropagation();
                        setExpanded((x) => ({ ...x, [task.id]: !isOpen }));
                      }}
                    >
                      ▶
                    </span>
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
                </div>
              </div>

              {multi && isOpen && task.checkouts.map((c) => {
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
                        void removeRepo(c.id, c.project_name, d);
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
          );
        })}

      </div>

      {creating && (
        <Modal
          title="New task"
          onClose={() => setCreating(false)}
          footer={
            <>
              <button className="btn" onClick={() => setCreating(false)}>Cancel</button>
              <button
                className="btn btn-primary"
                disabled={busy || !name.trim() || picked.length === 0}
                onClick={() => void createTask()}
              >
                {busy ? "Creating…" : `Create ${picked.length} worktree${picked.length === 1 ? "" : "s"}`}
              </button>
            </>
          }
        >
          <Field label="Task name">
            <input
              autoFocus
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="fix checkout rounding"
            />
          </Field>
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
          <Field label="Branch" hint="Defaults to a slug of the task name. Used in every repo.">
            <input
              value={branch}
              onChange={(e) => setBranch(e.target.value)}
              placeholder="(auto)"
            />
          </Field>
        </Modal>
      )}

      {addingRepoTo && (
        <Modal title={`Add a repo to ${addingRepoTo.name}`} onClose={() => setAddingRepoTo(null)}>
          <div className="muted" style={{ marginBottom: 10 }}>
            A worktree on <code>{addingRepoTo.branch}</code> is created beside the others.
          </div>
          {available(addingRepoTo).map((p) => (
            <div
              key={p.id}
              className="repo-pick"
              onClick={() => void addRepo(addingRepoTo, p.id)}
            >
              <span style={{ flex: 1 }}>{p.name}</span>
              <span className="path">{p.path}</span>
            </div>
          ))}
        </Modal>
      )}
    </div>
  );
}
