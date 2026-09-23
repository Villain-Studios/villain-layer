import { useEffect, useState } from "react";
import { api, errMessage } from "../lib/api";
import { taskTotals, useStore } from "../store";
import type { JiraTransition, TaskView } from "../lib/types";
import { Field, Modal, Spinner } from "./ui";

/**
 * Put away a task whose pull requests have all merged: its terminals, its
 * worktrees, its local branches, and its ticket.
 *
 * Merged tasks were left to pile up, each worktree with its dependencies
 * installed, until someone deleted them one by one — and then the ticket was
 * still in review on the board.
 */
export function FinishTask({ task, onClose }: { task: TaskView; onClose: () => void }) {
  const jiraConnected = useStore((s) => s.settings?.jira_connected ?? false);
  const paneCount = useStore((s) => s.panes.filter((p) => p.task_id === task.id).length);
  const rows = useStore((s) => s.prs[task.id]);
  const selected = useStore((s) => s.selectedTask);
  const select = useStore((s) => s.select);
  const refreshTasks = useStore((s) => s.refreshTasks);
  const refreshPanes = useStore((s) => s.refreshPanes);
  const toast = useStore((s) => s.toast);
  const fail = useStore((s) => s.fail);

  const key = jiraConnected ? task.issue_key : null;
  const [done, setDone] = useState<JiraTransition[] | null>(key ? null : []);
  const [ticketError, setTicketError] = useState<string | null>(null);
  const [transition, setTransition] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!key) return;
    let stop = false;
    api.jiraTransitions(key)
      .then((all) => {
        if (stop) return;
        // By category, never by name: "Done", "Closed" and "Erledigt" are all
        // the same place on some board.
        const to = all.filter((t) => t.to_category === "done");
        setDone(to);
        setTransition(to[0]?.id ?? "");
      })
      .catch((e) => {
        if (stop) return;
        setDone([]);
        setTicketError(errMessage(e));
      });
    return () => { stop = true; };
  }, [key]);

  // Work the merged PRs do not cover: committed after one landed, or in a
  // repo that never had one. Its branch is kept, and it is worth saying so
  // before rather than after.
  const uncovered = (rows ?? []).filter((r) => r.changed > 0);
  const landed = Object.fromEntries(
    (rows ?? []).filter((r) => r.pr?.merged && r.pr.head_sha).map((r) => [r.checkout_id, r.pr!.head_sha]),
  );
  const totals = taskTotals(task);
  const edited = totals.dirty + totals.staged;
  const repos = task.checkouts.map((c) => c.project_name);

  async function finish() {
    setBusy(true);
    try {
      const out = await api.finishTask(task.id, transition || null, landed);
      await Promise.all([refreshTasks(), refreshPanes()]);
      const stuck = out.repos.filter((r) => !r.ok);
      if (stuck.length > 0) {
        toast(
          "error",
          `Kept ${task.name} — git would not remove ${stuck.map((r) => `${r.repo} (${r.detail})`).join(", ")}`,
        );
        onClose();
        return;
      }
      if (selected === task.id) select(null);
      const moved = done?.find((t) => t.id === transition);
      toast(
        "success",
        `Finished ${task.name}` + (out.ticket_moved && moved ? ` · ${key} moved to ${moved.to_status}` : ""),
      );
      if (out.ticket_error) toast("error", `${key} could not be moved: ${out.ticket_error}`);
      const kept = out.branches.filter((b) => !b.ok);
      if (kept.length > 0) {
        toast("info", `Branch ${task.branch} was kept in ${kept.map((b) => `${b.repo} (${b.detail})`).join(", ")}`);
      }
      onClose();
    } catch (e) {
      fail(e);
      setBusy(false);
    }
  }

  return (
    <Modal
      title={`Finish ${task.issue_key ?? task.name}`}
      onClose={() => !busy && onClose()}
      footer={
        <>
          <button className="btn" onClick={onClose} disabled={busy}>Cancel</button>
          <button
            className="btn btn-primary"
            disabled={busy || done === null}
            onClick={() => void finish()}
          >
            {busy ? "Finishing…" : "Finish task"}
          </button>
        </>
      }
    >
      <p className="muted" style={{ marginTop: 0, lineHeight: 1.55, fontSize: 13 }}>
        Every pull request for <code>{task.branch}</code> has merged. This puts the task away:
      </p>
      <ul className="finish-list">
        {paneCount > 0 && (
          <li>Stops its {paneCount} terminal{paneCount === 1 ? "" : "s"}.</li>
        )}
        <li>
          Removes the worktree{repos.length === 1 ? "" : "s"} for {repos.join(", ")}, and the
          task folder.
        </li>
        <li>
          Deletes the local branch <code>{task.branch}</code> in each repository. The pull requests
          and what they landed stay on GitHub.
        </li>
      </ul>

      {uncovered.length > 0 && (
        <div className="confirm-detail" style={{ marginBottom: 12 }}>
          {uncovered.map((r) => r.repo).join(", ")}{" "}
          {uncovered.length === 1 ? "has" : "have"} work no merged pull request covers. The
          worktree still goes, but the branch is kept so the commits are not lost.
        </div>
      )}

      {edited > 0 && (
        <div className="confirm-detail" style={{ marginBottom: 12 }}>
          {edited} uncommitted change{edited === 1 ? "" : "s"} since the merge. git will not remove
          a worktree holding them, so the task will be kept — commit or discard them first.
        </div>
      )}

      {key && (
        <Field
          label="Ticket"
          hint={
            ticketError
              ? `Could not ask Jira where ${key} can go: ${ticketError}`
              : done && done.length === 0
                ? `Jira offers no move from here to a done status, so ${key} stays as it is.`
                : "Moved only once the worktrees are gone."
          }
        >
          {done === null ? (
            <div className="row muted"><Spinner /> Asking Jira…</div>
          ) : (
            <select
              value={transition}
              disabled={busy || done.length === 0}
              onChange={(e) => setTransition(e.target.value)}
            >
              {done.map((t) => (
                <option key={t.id} value={t.id}>
                  Move {key} to {t.to_status}
                  {t.name !== t.to_status ? ` (${t.name})` : ""}
                </option>
              ))}
              <option value="">Leave {key} as it is</option>
            </select>
          )}
        </Field>
      )}
    </Modal>
  );
}
