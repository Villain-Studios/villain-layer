import { useEffect, useState } from "react";
import { api, errMessage } from "../../lib/api";
import type { JiraTransition, TaskView } from "../../lib/types";
import { taskTotals, useStore } from "../../store";
import { flowOf, mergedTransition } from "../TicketFlow";
import { Modal } from "../ui";

/**
 * Delete a task, and say what becomes of its ticket (TKT-9). Deleting means
 * abandoned as often as done, so the ticket is left as it is, unless a pull
 * request of the task already merged, when closing it is the likely wish.
 * Either way the choice is on screen. Deleting used to leave tickets In
 * Progress for work merged weeks before.
 */
export function DeleteTask({ task, onCancel, onDelete }: {
  task: TaskView;
  onCancel: () => void;
  onDelete: (force: boolean, ticket: JiraTransition | null) => void;
}) {
  const settings = useStore((s) => s.settings);
  const rows = useStore((s) => s.prs[task.id]);
  const issue = useStore((s) => s.issues.find((i) => i.key === task.issue_key));
  const key = settings?.jira_connected ? task.issue_key : null;
  const merged = (rows ?? []).some((r) => r.pr?.merged);
  const [transitions, setTransitions] = useState<JiraTransition[] | null>(key ? null : []);
  const [ticketError, setTicketError] = useState<string | null>(null);
  const [pick, setPick] = useState("");

  // Once, as it opens: what merged and what was chosen are read then.
  useEffect(() => {
    if (!key) return;
    let live = true;
    api.jiraTransitions(key).then(
      (all) => {
        if (!live) return;
        setTransitions(all);
        if (merged) setPick(mergedTransition(all, flowOf(settings, key))?.id ?? "");
      },
      (e) => {
        if (!live) return;
        setTransitions([]);
        setTicketError(errMessage(e));
      },
    );
    return () => { live = false; };
  }, [key]);

  // Staged counts too: git refuses those as surely as unstaged ones, and
  // deleting without force then stopped and failed on a staged-only repo.
  const { dirty: unstaged, staged } = taskTotals(task);
  const dirty = unstaged + staged;
  const repos = task.checkouts.length;
  const chosen = transitions?.find((t) => t.id === pick) ?? null;

  return (
    <Modal
      title="Delete task"
      onClose={onCancel}
      footer={
        <>
          <button className="btn" onClick={onCancel}>Cancel</button>
          <button
            className="btn btn-danger-solid"
            autoFocus
            disabled={transitions === null}
            onClick={() => onDelete(dirty > 0, chosen)}
          >
            Delete task
          </button>
        </>
      }
    >
      <div style={{ lineHeight: 1.6 }}>
        Delete <b>{task.name}</b> and remove {repos} worktree{repos === 1 ? "" : "s"} from disk?
        <div className="muted" style={{ marginTop: 8 }}>
          Branch <code>{task.branch}</code> is left alone, in the repositories and on any remote.
        </div>
        {dirty > 0 && (
          <div className="confirm-detail">
            {dirty} uncommitted change{dirty === 1 ? "" : "s"} will be lost. This cannot be undone.
          </div>
        )}
        {key && (
          <div className="field" style={{ marginTop: 14, marginBottom: 0 }}>
            <label>Ticket {key}{issue ? ` · ${issue.status}` : ""}</label>
            {transitions === null ? (
              <div className="muted">Asking Jira where it can go…</div>
            ) : (
              <select value={pick} onChange={(e) => setPick(e.target.value)}>
                <option value="">Leave it as it is</option>
                {transitions.map((t) => (
                  <option key={t.id} value={t.id}>{t.name} → {t.to_status}</option>
                ))}
              </select>
            )}
            {ticketError && <div className="hint" style={{ color: "var(--red)" }}>{ticketError}</div>}
            {merged && !ticketError && <div className="hint">A pull request of this task has merged.</div>}
          </div>
        )}
      </div>
    </Modal>
  );
}
