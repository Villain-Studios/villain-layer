import type { MouseEvent } from "react";
import type { JiraIssue, JiraIssueType, JiraPage } from "../../lib/types";
import type { TypeMap } from "../IssueType";
import { TypeFilter } from "./TypeFilter";
import { TicketCard } from "./TicketCard";

export function BrowsePanel({
  issueTypes,
  types,
  kinds,
  onKinds,
  browseText,
  onBrowseText,
  whose,
  onWhose,
  includeDone,
  onIncludeDone,
  searching,
  found,
  onSearch,
  taskFor,
  syncing,
  onOpen,
  onSelectTask,
  onSync,
  onFileUnder,
  onContextMenu,
}: {
  issueTypes: JiraIssueType[];
  types: TypeMap;
  kinds: string[];
  onKinds: (kinds: string[]) => void;
  browseText: string;
  onBrowseText: (text: string) => void;
  whose: string;
  onWhose: (whose: string) => void;
  includeDone: boolean;
  onIncludeDone: (include: boolean) => void;
  searching: boolean;
  found: JiraPage | null;
  onSearch: () => void;
  taskFor: (key: string) => string | null;
  syncing: string | null;
  onOpen: (issue: JiraIssue) => void;
  onSelectTask: (taskId: string) => void;
  onSync: (key: string) => void;
  onFileUnder: (epic: { key: string; summary: string }) => void;
  onContextMenu: (e: MouseEvent, issue: JiraIssue) => void;
}) {
  return (
    <>
      <TypeFilter
        issueTypes={issueTypes}
        types={types}
        kinds={kinds}
        onChange={onKinds}
      />
      <div className="card browse-bar">
        <input
          type="text"
          autoFocus
          placeholder="Words to look for, or paste a key like ACME-21042…"
          value={browseText}
          onChange={(e) => onBrowseText(e.target.value)}
          onKeyDown={(e) => { if (e.key === "Enter") onSearch(); }}
        />
        <select value={whose} onChange={(e) => onWhose(e.target.value)}>
          <option value="notmine">Not mine</option>
          <option value="unassigned">Unassigned</option>
          <option value="anyone">Anyone</option>
        </select>
        <label className="row" style={{ gap: 6, cursor: "pointer", whiteSpace: "nowrap" }}>
          <input
            type="checkbox"
            style={{ width: "auto" }}
            checked={includeDone}
            onChange={(e) => onIncludeDone(e.target.checked)}
          />
          Include done
        </label>
        <button
          className="btn btn-sm btn-primary"
          disabled={searching}
          onClick={onSearch}
        >
          {searching ? "Searching…" : "Search"}
        </button>
      </div>

      {found && found.issues.length === 0 && !searching && (
        <div className="card">
          <div className="muted">
            Nothing matched. "Not mine" covers unassigned work as well as other
            people's, so widening it rarely helps — try fewer words, or turn on
            <b> Include done</b>.
          </div>
        </div>
      )}
      {found && found.issues.length > 0 && (
        <>
          <div className="found">
            {found.issues.map((issue) => (
              <TicketCard
                key={issue.key}
                issue={issue}
                types={types}
                taskId={taskFor(issue.key)}
                syncing={syncing === issue.key}
                onOpen={() => onOpen(issue)}
                onSelectTask={onSelectTask}
                onSync={() => onSync(issue.key)}
                onFileUnder={() => onFileUnder({ key: issue.key, summary: issue.summary })}
                onContextMenu={(e) => {
                  e.preventDefault();
                  onContextMenu(e, issue);
                }}
              />
            ))}
          </div>
          {found.more && (
            <div className="muted" style={{ marginTop: 10, fontSize: 12.5 }}>
              Jira had more than these. Add a word to the search, or narrow it
              by type.
            </div>
          )}
        </>
      )}
    </>
  );
}
