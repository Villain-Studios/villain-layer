import { useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api } from "../../lib/api";
import { useStore } from "../../store";
import type { JiraIssue, JiraTransition } from "../../lib/types";
import { Field, Modal } from "../ui";
import { IssueTypeIcon, type TypeMap } from "../IssueType";
import { statusClass } from "./status";

export function ReadTicketDialog({
  issue,
  types,
  hasTask,
  onClose,
  onStart,
  onMoved,
}: {
  issue: JiraIssue;
  types: TypeMap;
  hasTask: boolean;
  onClose: () => void;
  onStart: () => void;
  onMoved: (toStatus: string) => void;
}) {
  const fail = useStore((s) => s.fail);
  const [transitions, setTransitions] = useState<JiraTransition[]>([]);
  const [moving, setMoving] = useState(false);

  // Keyed on the status as well as the ticket: which moves are on offer
  // depends on where it is now, and after a move the old list offered the
  // ones valid from before, which Jira then refused.
  useEffect(() => {
    let current = true;
    setTransitions([]);
    api.jiraTransitions(issue.key)
      .then((t) => { if (current) setTransitions(t); })
      .catch(() => { if (current) setTransitions([]); });
    return () => { current = false; };
  }, [issue.key, issue.status]);

  async function doTransition(t: JiraTransition) {
    setMoving(true);
    try {
      await api.jiraTransition(issue.key, t.id);
      onMoved(t.to_status);
    } catch (e) {
      fail(e);
    } finally {
      setMoving(false);
    }
  }

  return (
    <Modal
      title={issue.key}
      wide
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>Close</button>
          <button
            className="btn btn-sm"
            onClick={() => void openUrl(issue.url).catch(fail)}
            title="The same ticket, on the website"
          >
            Open in Jira ↗
          </button>
          <div className="spacer" />
          {!hasTask && (
            <button className="btn btn-primary" onClick={onStart}>
              Start work…
            </button>
          )}
        </>
      }
    >
      <h3 style={{ margin: "0 0 10px", fontSize: 15, lineHeight: 1.4 }}>
        {issue.summary}
      </h3>

      <div className="row" style={{ marginBottom: 14, flexWrap: "wrap" }}>
        <span className="chip chip-type">
          <IssueTypeIcon types={types} name={issue.issue_type} size={13} />
          {issue.issue_type}
        </span>
        <span className={`status-pill ${statusClass(issue.status_category)}`}>
          {issue.status}
        </span>
        {issue.priority && <span className="chip">{issue.priority}</span>}
        {issue.assignee && <span className="chip">{issue.assignee}</span>}
        {issue.epic_key && (
          <span className="chip" title={issue.epic_summary ?? undefined}>
            epic {issue.epic_key}
          </span>
        )}
        {issue.components.map((c) => <span key={c} className="chip">{c}</span>)}
        {issue.labels.map((l) => <span key={l} className="chip">{l}</span>)}
      </div>

      {transitions.length > 0 && (
        <Field
          label="Move ticket"
          hint="Whatever this workflow allows from the current status — including out of Review."
        >
          <div className="row" style={{ flexWrap: "wrap" }}>
            {transitions.map((t) => (
              <button
                key={t.id}
                className="btn btn-sm"
                disabled={moving}
                onClick={() => void doTransition(t)}
              >
                {t.name}
              </button>
            ))}
          </div>
        </Field>
      )}

      {/*
        Jira's own markup is reduced to text on the way in, so it is shown
        as text: wrapped, spacing kept, and never pretending to be the
        rendering the website would give it.
      */}
      {issue.description.trim() ? (
        <div className="ticket-body">{issue.description}</div>
      ) : (
        <div className="muted">This ticket has no description.</div>
      )}
    </Modal>
  );
}
