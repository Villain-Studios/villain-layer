import type { MouseEvent } from "react";
import type { JiraIssue } from "../../lib/types";
import { age } from "../../lib/time";
import { IssueTypeIcon, hierarchyClass, isEpicType, type TypeMap } from "../IssueType";
import { statusClass } from "./status";

/// One ticket, wherever it is being listed: your own epics or a search.
export function TicketCard({
  issue,
  types,
  taskId,
  syncing,
  focused,
  onOpen,
  onSelectTask,
  onSync,
  onFileUnder,
  onContextMenu,
}: {
  issue: JiraIssue;
  types: TypeMap;
  taskId: string | null;
  syncing: boolean;
  focused?: boolean;
  onOpen: () => void;
  onSelectTask: (taskId: string) => void;
  onSync: () => void;
  onFileUnder: () => void;
  onContextMenu: (e: MouseEvent) => void;
}) {
  // Jira's middle category: whatever this workflow calls being under way.
  const moving = issue.status_category === "indeterminate";
  return (
    <div
      data-issue-key={issue.key}
      className={
        `ticket${taskId ? " started" : ""}` +
        (focused ? " focused" : "") +
        (isEpicType(types, issue.issue_type) ? " is-epic" : "") +
        ` ${hierarchyClass(types, issue.issue_type)}`
      }
      title={taskId ? "Open the task already running for this ticket" : undefined}
      onContextMenu={onContextMenu}
      onClick={() => {
        // Already being worked on: go there rather than offering to start it
        // a second time on the same branch.
        if (taskId) onSelectTask(taskId);
        else onOpen();
      }}
    >
      <div className="top">
        <IssueTypeIcon types={types} name={issue.issue_type} />
        <span className="key-chip">{issue.key}</span>
        <span className={`status-pill ${statusClass(issue.status_category)}`}>
          {issue.status}
        </span>
        {/*
          A ticket with a task here says so, with the one action that goes
          with it. One with no task has no chip: clicking the card offers
          Start work.
        */}
        {taskId && moving && <span className="chip add">open task →</span>}
        {taskId && !moving && (
          // A branch and an agent are running against a ticket the board
          // still calls Open. Say so, and offer the click that fixes it.
          <button
            className="chip warn sync"
            disabled={syncing}
            title={`Move ${issue.key} into progress in Jira`}
            onClick={(e) => { e.stopPropagation(); onSync(); }}
          >
            {syncing ? "moving…" : "started here — sync ↑"}
          </button>
        )}
        {isEpicType(types, issue.issue_type) && (
          <button
            className="chip sync open"
            title={`File a new ticket under ${issue.key}`}
            onClick={(e) => {
              e.stopPropagation();
              onFileUnder();
            }}
          >
            + ticket
          </button>
        )}
      </div>
      <div className="summary">{issue.summary}</div>
      <div className="bottom">
        <span>{issue.issue_type}</span>
        {issue.priority && <span>· {issue.priority}</span>}
        <span className={issue.assignee ? "" : "free"}>
          · {issue.assignee ?? "unassigned"}
        </span>
        {issue.components.map((c) => (
          <span key={c} className="group-chip">{c}</span>
        ))}
        {issue.created && (
          <span className="age" title={`Created ${new Date(issue.created).toLocaleString()}`}>
            {age(issue.created)}
          </span>
        )}
      </div>
    </div>
  );
}
