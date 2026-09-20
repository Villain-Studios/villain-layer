import { useEffect, useMemo, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api } from "../lib/api";
import { groupByEpic, useStore } from "../store";
import type { CreateField, JiraIssue, JiraPage, JiraTransition } from "../lib/types";
import { ContextMenu, Field, Modal, Spinner, type MenuItem } from "./ui";
import { RepoPicker } from "./RepoPicker";
import { IssueTypeIcon, hierarchyClass, isEpicType, typeMap } from "./IssueType";

function statusClass(category: string) {
  if (category === "done") return "done";
  if (category === "indeterminate") return "indeterminate";
  return "new";
}

export function TicketsView() {
  const { issues, issueTypes, issuesLoading, issuesTruncated, settings, projects, agents, tasks } =
    useStore();
  const refreshIssues = useStore((s) => s.refreshIssues);
  const refreshTasks = useStore((s) => s.refreshTasks);
  const refreshPanes = useStore((s) => s.refreshPanes);
  const toggleSettings = useStore((s) => s.toggleSettings);
  const select = useStore((s) => s.select);
  const toast = useStore((s) => s.toast);
  const fail = useStore((s) => s.fail);

  const [open, setOpen] = useState<JiraIssue | null>(null);
  // Reading a ticket is not starting one. Jira already sent the description
  // with the rest of the issue, so this costs nothing but a dialog.
  const [reading, setReading] = useState<JiraIssue | null>(null);
  const [transitions, setTransitions] = useState<JiraTransition[]>([]);
  const [picked, setPicked] = useState<string[]>([]);
  const [reason, setReason] = useState<string | null>(null);
  const [agentId, setAgentId] = useState("");
  const [suffix, setSuffix] = useState("");
  const [starting, setStarting] = useState(false);
  const [shut, setShut] = useState<Record<string, boolean>>({});
  const [query, setQuery] = useState("");
  const [sub, setSub] = useState<"mine" | "browse">("mine");
  const [browseText, setBrowseText] = useState("");
  const [whose, setWhose] = useState("notmine");
  const [includeDone, setIncludeDone] = useState(false);
  const [found, setFound] = useState<JiraPage | null>(null);
  const [searching, setSearching] = useState(false);
  // Shared by both tabs: your own list filters in place, a search asks Jira.
  const [kinds, setKinds] = useState<string[]>([]);
  const [syncing, setSyncing] = useState<string | null>(null);
  // Held as built items rather than a subject, so an epic header and a ticket
  // card can each offer what makes sense for them.
  const [menu, setMenu] = useState<{ x: number; y: number; items: MenuItem[] } | null>(null);
  // Filing under an epic: the parent is fixed, everything else is asked for.
  const [filing, setFiling] = useState<{ key: string; summary: string } | null>(null);
  const [newSummary, setNewSummary] = useState("");
  const [newDesc, setNewDesc] = useState("");
  const [newType, setNewType] = useState("");
  const [filingBusy, setFilingBusy] = useState(false);
  // What this project insists on, asked of Jira rather than assumed.
  const [needed, setNeeded] = useState<CreateField[]>([]);
  const [neededLoading, setNeededLoading] = useState(false);
  const [extra, setExtra] = useState<Record<string, string[]>>({});

  const installed = agents.filter((a) => a.installed);
  const jiraBase = settings?.jira?.base_url.replace(/\/+$/, "") ?? "";
  const types = useMemo(() => typeMap(issueTypes), [issueTypes]);

  useEffect(() => {
    if (!agentId && installed.length) setAgentId(installed[0].id);
  }, [installed, agentId]);

  useEffect(() => {
    if (!open) { setTransitions([]); setReason(null); return; }
    setSuffix("");
    api.jiraTransitions(open.key).then(setTransitions).catch(() => setTransitions([]));
    api.suggestRepos({ issueKey: open.key, epicKey: open.epic_key })
      .then((s) => { setPicked(s.project_ids); setReason(s.reason); })
      .catch(() => { setPicked([]); setReason(null); });
  }, [open]);

  // Narrowing by type or by who holds it is a different question for Jira, so
  // it is asked again. The text box is not: searching on every keystroke would
  // be a request per character.
  useEffect(() => {
    if (sub !== "browse" || found === null) return;
    void browse();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [kinds, whose, includeDone]);

  useEffect(() => {
    if (sub === "browse" && found === null) void browse();
    // Only on entering the tab: re-running on every state change would search
    // Jira on each keystroke.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sub]);

  // What a new ticket can be: a plain issue, never an epic or a sub-task —
  // one would be a sibling of the parent, the other needs a parent of its own.
  const creatable = useMemo(
    () => issueTypes.filter((t) => t.hierarchy_level === 0 && !t.subtask),
    [issueTypes],
  );

  useEffect(() => {
    if (newType && creatable.some((t) => t.name === newType)) return;
    setNewType((creatable.find((t) => t.name.toLowerCase() === "task") ?? creatable[0])?.name ?? "");
  }, [creatable, newType]);

  useEffect(() => {
    const project = filing?.key.split("-")[0];
    const typeId = creatable.find((t) => t.name === newType)?.id;
    if (!project || !typeId) { setNeeded([]); return; }
    setNeededLoading(true);
    setExtra({});
    api.jiraCreateFields(project, typeId)
      .then((f) => setNeeded(f.filter((x) => x.required)))
      // A site that will not describe its own form is no reason to block the
      // dialog: Jira still says what is missing if the create is refused.
      .catch(() => setNeeded([]))
      .finally(() => setNeededLoading(false));
  }, [filing, newType, creatable]);

  /// Shaped the way Jira wants each field, from the metadata it gave us.
  function extraFields(): Record<string, unknown> {
    const out: Record<string, unknown> = {};
    for (const field of needed) {
      const picked = extra[field.id] ?? [];
      if (picked.length === 0) continue;
      out[field.id] =
        field.kind === "array" ? picked.map((id) => ({ id })) : { id: picked[0] };
    }
    return out;
  }

  // Fields the dialog fills itself, whether or not Jira calls them required.
  const OWN = ["summary", "description", "issuetype", "project", "parent", "reporter"];
  // Required, and a closed set of values, so it can be offered as a choice.
  const pickable = needed.filter((f) => !OWN.includes(f.id) && f.allowed.length > 0);
  // Required, free-form, and not something this dialog asks for. Nothing
  // sensible can be invented for these, so say so rather than failing at Jira.
  const unsupported = needed.filter((f) => !OWN.includes(f.id) && f.allowed.length === 0);
  const descriptionRequired = needed.some((f) => f.id === "description");

  const missing = [
    ...pickable.filter((f) => (extra[f.id] ?? []).length === 0),
    ...(descriptionRequired && !newDesc.trim()
      ? [{ id: "description", name: "Description" }]
      : []),
  ];

  async function fileIssue() {
    if (!filing || !newSummary.trim() || !newType) return;
    setFilingBusy(true);
    try {
      const issue = await api.jiraCreateIssue({
        summary: newSummary.trim(),
        description: newDesc.trim(),
        issue_type: newType,
        project_key: null,
        parent_key: filing.key,
        fields: extraFields(),
      });
      toast("success", `Filed ${issue.key} under ${filing.key}`);
      setFiling(null);
      setNewSummary("");
      setNewDesc("");
      await refreshIssues();
      // Straight into the start-work dialog: filing it is usually the first
      // half of picking it up.
      setOpen(issue);
    } catch (e) {
      fail(e);
    } finally {
      setFilingBusy(false);
    }
  }

  function copy(text: string, what: string) {
    navigator.clipboard
      .writeText(text)
      .then(() => toast("success", `Copied ${what}`))
      .catch(() => toast("error", "Could not reach the clipboard"));
  }

  /// Opening it in Jira and taking its link are the same wherever it is shown,
  /// and an epic known only through its children still has both.
  function epicItems(key: string, summary: string): MenuItem[] {
    return [
      {
        label: "New ticket in this epic…",
        onSelect: () => { setFiling({ key, summary }); setNewSummary(""); setNewDesc(""); },
      },
    ];
  }

  function linkItems(key: string, summary: string, after = true): MenuItem[] {
    const link = `${jiraBase}/browse/${key}`;
    return [
      // A divider only when something precedes them.
      { label: "Open in Jira", separated: after, onSelect: () => void openUrl(link).catch(fail) },
      { label: "Copy link", onSelect: () => copy(link, link) },
      { label: `Copy ${key}`, onSelect: () => copy(key, key) },
      { label: "Copy summary", onSelect: () => copy(`${key} ${summary}`, key) },
    ];
  }

  /// What you can do with a ticket without opening it.
  function issueMenu(issue: JiraIssue): MenuItem[] {
    const task = taskFor(issue.key);
    const items: MenuItem[] = [
      {
        label: task ? "Open task" : "Start work…",
        onSelect: () => (task ? select(task.id) : setOpen(issue)),
      },
      { label: "Read ticket", onSelect: () => setReading(issue) },
      ...(isEpicType(types, issue.issue_type) ? epicItems(issue.key, issue.summary) : []),
      ...linkItems(issue.key, issue.summary),
    ];
    if (task && issue.status_category !== "indeterminate") {
      items.push({
        label: "Move to in progress",
        separated: true,
        onSelect: () => void syncStatus(issue.key),
      });
    }
    return items;
  }

  /// Bring a ticket back into step with the work already happening here.
  async function syncStatus(key: string) {
    setSyncing(key);
    try {
      const moved = await api.jiraSyncStatus(key);
      if (moved) {
        toast("success", `${key} → ${moved}`);
        await refreshIssues();
      } else {
        toast("info", `${key} offers no transition into progress.`);
      }
    } catch (e) {
      fail(e);
    } finally {
      setSyncing(null);
    }
  }

  async function browse() {
    setSearching(true);
    try {
      setFound(await api.jiraBrowse(browseText.trim(), whose, includeDone, kinds));
    } catch (e) {
      setFound({ issues: [], more: false });
      fail(e);
    } finally {
      setSearching(false);
    }
  }

  async function startWork() {
    if (!open || picked.length === 0) return;
    setStarting(true);
    try {
      const task = await api.jiraStartWork(open.key, picked, agentId || null, suffix || null);
      // Refresh the issues too: the ticket has usually just moved, and the
      // status on the card is the thing the move was meant to correct.
      await Promise.all([refreshTasks(), refreshPanes(), refreshIssues()]);
      select(task.id);
      setOpen(null);
      toast(
        "success",
        `${picked.length} worktree${picked.length === 1 ? "" : "s"} ready on ${task.branch}` +
          (task.moved ? ` · ${open.key} → ${task.moved}` : ""),
      );
    } catch (e) {
      fail(e);
    } finally {
      setStarting(false);
    }
  }

  async function doTransition(t: JiraTransition) {
    if (!open) return;
    try {
      await api.jiraTransition(open.key, t.id);
      toast("success", `${open.key} → ${t.to_status}`);
      setOpen(null);
      await refreshIssues();
    } catch (e) {
      fail(e);
    }
  }

  if (!settings?.jira_connected) {
    return (
      <div className="empty">
        <h2>Jira not connected</h2>
        <p>Connect Jira to pull your assigned issues and start work from a ticket.</p>
        <button className="btn" onClick={() => toggleSettings(true)}>Connect Jira</button>
      </div>
    );
  }

  /// Issue types as toggles. Nothing picked means every type — the same thing
  /// as picking them all, and less work to say.
  function typeFilter() {
    if (issueTypes.length === 0) return null;
    return (
      <div className="type-filter">
        {issueTypes
          .filter((t) => !t.subtask)
          .map((t) => {
            const on = kinds.includes(t.name);
            return (
              <button
                key={t.id}
                className={`type-pick${on ? " active" : ""}`}
                onClick={() =>
                  setKinds((c) =>
                    c.includes(t.name) ? c.filter((n) => n !== t.name) : [...c, t.name],
                  )
                }
              >
                <IssueTypeIcon types={types} name={t.name} size={14} />
                {t.name}
              </button>
            );
          })}
        {kinds.length > 0 && (
          <button
            className="type-pick clear"
            onClick={() => setKinds([])}
          >
            Clear
          </button>
        )}
      </div>
    );
  }

  /// One ticket, wherever it is being listed: your own epics or a search.
  function ticketCard(issue: JiraIssue) {
    const task = taskFor(issue.key);
    // Jira's middle category: whatever this workflow calls being under way.
    const moving = issue.status_category === "indeterminate";
    return (
      <div
        key={issue.key}
        className={
          `ticket${task ? " started" : ""}` +
          (isEpicType(types, issue.issue_type) ? " is-epic" : "") +
          ` ${hierarchyClass(types, issue.issue_type)}`
        }
        title={task ? "Open the task already running for this ticket" : undefined}
        onContextMenu={(e) => {
          e.preventDefault();
          setMenu({ x: e.clientX, y: e.clientY, items: issueMenu(issue) });
        }}
        onClick={() => {
          // Already being worked on: go there rather than offering to start it
          // a second time on the same branch.
          if (task) select(task.id);
          else setOpen(issue);
        }}
      >
        <div className="top">
          <IssueTypeIcon types={types} name={issue.issue_type} />
          <span className="key-chip">{issue.key}</span>
          <span className={`status-pill ${statusClass(issue.status_category)}`}>
            {issue.status}
          </span>
          {/*
            Every ticket that is moving says where it stands here, because the
            absence of a chip reads as "fine" rather than as "nothing set up".
            Three states, each with the one action that resolves it.
          */}
          {task && moving && <span className="chip add">open task →</span>}
          {task && !moving && (
            // A branch and an agent are running against a ticket the board
            // still calls Open. Say so, and offer the click that fixes it.
            <button
              className="chip warn sync"
              disabled={syncing === issue.key}
              title={`Move ${issue.key} into progress in Jira`}
              onClick={(e) => { e.stopPropagation(); void syncStatus(issue.key); }}
            >
              {syncing === issue.key ? "moving…" : "started here — sync ↑"}
            </button>
          )}
          {isEpicType(types, issue.issue_type) && (
            <button
              className="chip sync open"
              title={`File a new ticket under ${issue.key}`}
              onClick={(e) => {
                e.stopPropagation();
                setFiling({ key: issue.key, summary: issue.summary });
                setNewSummary("");
                setNewDesc("");
              }}
            >
              + ticket
            </button>
          )}
          {!task && moving && !isEpicType(types, issue.issue_type) && (
            // In progress on the board with nothing here to work in.
            <button
              className="chip sync open"
              title={`Create worktrees and start work on ${issue.key}`}
              onClick={(e) => { e.stopPropagation(); setOpen(issue); }}
            >
              no worktree — start ↓
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
        </div>
      </div>
    );
  }

  const q = query.trim().toLowerCase();
  const matchesKind = (i: JiraIssue) => kinds.length === 0 || kinds.includes(i.issue_type);
  const filtered = issues.filter(
    (i) =>
      matchesKind(i) &&
      (!q ||
        i.key.toLowerCase().includes(q) ||
        i.summary.toLowerCase().includes(q) ||
        i.labels.some((l) => l.toLowerCase().includes(q)) ||
        i.components.some((c) => c.toLowerCase().includes(q))),
  );
  const taskFor = (key: string) => tasks.find((t) => t.issue_key === key);
  const started = new Set(
    tasks.map((t) => t.issue_key).filter((k): k is string => !!k),
  );
  const epics = groupByEpic(filtered, started);
  // Collapsed is stored per epic, so "all" means every one currently listed —
  // which is what a filter has narrowed things to, not the whole board.
  const allShut = epics.length > 0 && epics.every((e) => shut[e.key] ?? false);

  return (
    <div className="wide">
      <div className="wide-head">
        <h2>Tickets</h2>
        <div className="subtabs">
          <button
            className={sub === "mine" ? "active" : ""}
            onClick={() => setSub("mine")}
          >
            Mine<span className="badge">{issues.length}</span>
          </button>
          <button
            className={sub === "browse" ? "active" : "call"}
            onClick={() => setSub("browse")}
            title="Search the whole board, not just what is assigned to you"
          >
            🔍 Find work
          </button>
        </div>
        <div className="spacer" />
        {sub === "mine" ? (
          <>
            <span className="sub">
              {filtered.length} issue{filtered.length === 1 ? "" : "s"} in {epics.length} epic
              {epics.length === 1 ? "" : "s"}
            </span>
            {issuesLoading && <Spinner />}
            <input
              type="text"
              style={{ width: 220 }}
              placeholder="Filter by key, title, label…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
            <button
              className="btn btn-sm"
              disabled={epics.length === 0}
              title={allShut ? "Open every epic" : "Close every epic"}
              onClick={() =>
                setShut(
                  allShut ? {} : Object.fromEntries(epics.map((e) => [e.key, true])),
                )
              }
            >
              {allShut ? "Expand all" : "Collapse all"}
            </button>
            <button className="btn btn-sm" onClick={() => void refreshIssues()}>Refresh</button>
          </>
        ) : (
          <span className="sub">
            {found
              ? `${found.issues.length}${found.more ? "+" : ""} result${
                  found.issues.length === 1 ? "" : "s"
                }`
              : "Not searched yet"}
          </span>
        )}
      </div>

      {sub === "browse" && (
        <>
          {typeFilter()}
          <div className="card browse-bar">
            <input
              type="text"
              autoFocus
              placeholder="Words to look for, or paste a key like ACME-21042…"
              value={browseText}
              onChange={(e) => setBrowseText(e.target.value)}
              onKeyDown={(e) => { if (e.key === "Enter") void browse(); }}
            />
            <select value={whose} onChange={(e) => setWhose(e.target.value)}>
              <option value="notmine">Not mine</option>
              <option value="unassigned">Unassigned</option>
              <option value="anyone">Anyone</option>
            </select>
            <label className="row" style={{ gap: 6, cursor: "pointer", whiteSpace: "nowrap" }}>
              <input
                type="checkbox"
                style={{ width: "auto" }}
                checked={includeDone}
                onChange={(e) => setIncludeDone(e.target.checked)}
              />
              Include done
            </label>
            <button
              className="btn btn-sm btn-primary"
              disabled={searching}
              onClick={() => void browse()}
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
              <div className="found">{found.issues.map(ticketCard)}</div>
              {found.more && (
                <div className="muted" style={{ marginTop: 10, fontSize: 12.5 }}>
                  Jira had more than these. Add a word to the search, or narrow it
                  by type.
                </div>
              )}
            </>
          )}
        </>
      )}

      {sub === "mine" && typeFilter()}

      {sub === "mine" && issuesTruncated && (
        <div className="card">
          <div className="muted">
            Your JQL matches more than the {issues.length} shown, so the epics below
            are missing some of their tickets. Narrow it in Settings.
          </div>
        </div>
      )}

      {sub === "mine" && epics.length === 0 && !issuesLoading && (
        <div className="card">
          <div className="muted">
            {q ? `Nothing matches “${query}”.` : "No issues matched your JQL. Adjust it in Settings."}
          </div>
        </div>
      )}

      {sub === "mine" && epics.map((epic) => {
        const closed = shut[epic.key] ?? false;
        // Name the epic's own type from the site rather than assuming "Epic":
        // some Jiras rename the level-1 type.
        const epicTypeName =
          issues.find((i) => i.key === epic.key)?.issue_type ??
          issueTypes.find((t) => t.hierarchy_level >= 1)?.name ??
          "Epic";
        return (
          <div key={epic.key || "_none"} className="epic">
            <div
              className="epic-head"
              onClick={() => setShut((c) => ({ ...c, [epic.key]: !closed }))}
              onContextMenu={(e) => {
                if (!epic.key) return;
                e.preventDefault();
                const own = epic.issue;
                setMenu({
                  x: e.clientX,
                  y: e.clientY,
                  items: [
                    ...(own
                      ? [{
                          label: taskFor(own.key) ? "Open task" : "Start work…",
                          onSelect: () =>
                            taskFor(own.key)
                              ? select(taskFor(own.key)!.id)
                              : setOpen(own),
                        }]
                      : []),
                    ...epicItems(epic.key, epic.summary),
                    ...linkItems(epic.key, epic.summary, true),
                  ],
                });
              }}
            >
              <span className={`chev${closed ? "" : " open"}`}>▶</span>
              {epic.key && <IssueTypeIcon types={types} name={epicTypeName} size={18} />}
              {epic.key ? <span className="key-chip">{epic.key}</span> : null}
              <span className="title">{epic.summary || (epic.key ? epic.key : "No epic")}</span>
              <span className="count">{epic.issues.length}</span>
              {epic.live > 0 && (
                <span className="chip add" title="Tickets in progress, or with a task open here">
                  {epic.live} in flight
                </span>
              )}
              <div className="spacer" />
              {/*
                Working on the epic itself is only possible when it is one of
                your own issues: otherwise all the app has is the key and title
                its children carry, which is not enough to start from.
              */}
              {epic.issue && (
                <button
                  className="btn btn-sm"
                  onClick={(e) => {
                    e.stopPropagation();
                    const task = taskFor(epic.issue!.key);
                    if (task) select(task.id);
                    else setOpen(epic.issue!);
                  }}
                >
                  {taskFor(epic.issue.key) ? "Open task" : "Start work"}
                </button>
              )}
              {/*
                Jira itself is always reachable: the key is enough for a link,
                so every epic gets the same way out rather than some of them
                appearing actionable and the rest not.
              */}
              {epic.key && (
                <button
                  className="btn btn-sm"
                  title={`File a new ticket under ${epic.key}`}
                  onClick={(e) => {
                    e.stopPropagation();
                    setFiling({ key: epic.key, summary: epic.summary });
                    setNewSummary("");
                    setNewDesc("");
                  }}
                >
                  + ticket
                </button>
              )}
              {epic.key && jiraBase && (
                <button
                  className="btn btn-sm"
                  title={`Open ${epic.key} in Jira`}
                  onClick={(e) => {
                    e.stopPropagation();
                    void openUrl(`${jiraBase}/browse/${epic.key}`).catch(fail);
                  }}
                >
                  Jira ↗
                </button>
              )}
            </div>

            {!closed && (
              <div className="epic-body">
                {epic.issues.map(ticketCard)}
              </div>
            )}
          </div>
        );
      })}

      {filing && (
        <Modal
          title={`New ticket in ${filing.key}`}
          onClose={() => setFiling(null)}
          footer={
            <>
              <button className="btn" onClick={() => setFiling(null)}>Cancel</button>
              <button
                className="btn btn-primary"
                disabled={filingBusy || !newSummary.trim() || !newType || missing.length > 0}
                title={
                  missing.length > 0
                    ? `${missing.map((f) => f.name).join(", ")} required by this project`
                    : undefined
                }
                onClick={() => void fileIssue()}
              >
                {filingBusy ? "Filing…" : "File ticket"}
              </button>
            </>
          }
        >
          <div className="muted" style={{ marginBottom: 12, lineHeight: 1.6 }}>
            Filed under <b>{filing.key}</b>
            {filing.summary ? ` — ${filing.summary}` : ""}, in that epic's own project.
            Nothing is checked out; the start-work dialog opens once it exists.
          </div>

          <Field label="Summary">
            <input
              autoFocus
              value={newSummary}
              onChange={(e) => setNewSummary(e.target.value)}
              placeholder="What needs doing"
              onKeyDown={(e) => { if (e.key === "Enter") void fileIssue(); }}
            />
          </Field>

          <Field label="Type">
            <div className="type-row">
              {creatable.map((t) => (
                <button
                  key={t.id}
                  className={`type-pick${newType === t.name ? " active" : ""}`}
                  onClick={() => setNewType(t.name)}
                >
                  <IssueTypeIcon types={types} name={t.name} size={15} />
                  {t.name}
                </button>
              ))}
            </div>
          </Field>

          {neededLoading && (
            <div className="muted" style={{ marginBottom: 10 }}>
              Asking Jira what this project requires…
            </div>
          )}
          {unsupported.length > 0 && (
            <div className="confirm-detail" style={{ marginBottom: 12 }}>
              This project also requires {unsupported.map((f) => f.name).join(", ")},
              which this dialog cannot fill in. Jira will refuse the ticket — file it
              in Jira instead, and it will show up here on the next refresh.
            </div>
          )}
          {pickable.map((field) => {
            const picked = extra[field.id] ?? [];
            const many = field.kind === "array";
            return (
              <Field
                key={field.id}
                label={field.name}
                hint={`Required by this project${many ? " — pick one or more" : ""}.`}
              >
                <div className="type-row">
                  {field.allowed.map((v) => {
                    const on = picked.includes(v.id);
                    return (
                      <button
                        key={v.id}
                        className={`type-pick${on ? " active" : ""}`}
                        onClick={() =>
                          setExtra((c) => ({
                            ...c,
                            [field.id]: on
                              ? picked.filter((x) => x !== v.id)
                              : many
                                ? [...picked, v.id]
                                : [v.id],
                          }))
                        }
                      >
                        {v.name}
                      </button>
                    );
                  })}
                </div>
              </Field>
            );
          })}

          <Field
            label="Description"
            hint={
              descriptionRequired
                ? "Required by this project. Becomes the ticket body and an agent's briefing."
                : "Optional. Becomes the ticket body and an agent's briefing."
            }
          >
            <textarea
              rows={5}
              value={newDesc}
              onChange={(e) => setNewDesc(e.target.value)}
              placeholder="What needs doing, and how you would know it is done."
            />
          </Field>
        </Modal>
      )}

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          items={menu.items}
          onClose={() => setMenu(null)}
        />
      )}

      {reading && (
        <Modal
          title={reading.key}
          wide
          onClose={() => setReading(null)}
          footer={
            <>
              <button className="btn" onClick={() => setReading(null)}>Close</button>
              <button
                className="btn btn-sm"
                onClick={() => void openUrl(reading.url)}
                title="The same ticket, on the website"
              >
                Open in Jira ↗
              </button>
              <div className="spacer" />
              {!taskFor(reading.key) && (
                <button
                  className="btn btn-primary"
                  onClick={() => { setOpen(reading); setReading(null); }}
                >
                  Start work…
                </button>
              )}
            </>
          }
        >
          <h3 style={{ margin: "0 0 10px", fontSize: 15, lineHeight: 1.4 }}>
            {reading.summary}
          </h3>

          <div className="row" style={{ marginBottom: 14, flexWrap: "wrap" }}>
            <span className="chip chip-type">
              <IssueTypeIcon types={types} name={reading.issue_type} size={13} />
              {reading.issue_type}
            </span>
            <span className={`status-pill ${statusClass(reading.status_category)}`}>
              {reading.status}
            </span>
            {reading.priority && <span className="chip">{reading.priority}</span>}
            {reading.assignee && <span className="chip">{reading.assignee}</span>}
            {reading.epic_key && (
              <span className="chip" title={reading.epic_summary ?? undefined}>
                epic {reading.epic_key}
              </span>
            )}
            {reading.components.map((c) => <span key={c} className="chip">{c}</span>)}
            {reading.labels.map((l) => <span key={l} className="chip">{l}</span>)}
          </div>

          {/*
            Jira's own markup is reduced to text on the way in, so it is shown
            as text: wrapped, spacing kept, and never pretending to be the
            rendering the website would give it.
          */}
          {reading.description.trim() ? (
            <div className="ticket-body">{reading.description}</div>
          ) : (
            <div className="muted">This ticket has no description.</div>
          )}
        </Modal>
      )}

      {open && (
        <Modal
          title={`${open.key} · ${open.status}`}
          wide
          onClose={() => setOpen(null)}
          footer={
            <>
              <button className="btn" onClick={() => setOpen(null)}>Close</button>
              <button
                className="btn btn-primary"
                disabled={picked.length === 0 || starting}
                onClick={() => void startWork()}
              >
                {starting
                  ? "Creating worktrees…"
                  : `Start work in ${picked.length} repo${picked.length === 1 ? "" : "s"}`}
              </button>
            </>
          }
        >
          <h3 style={{ margin: "0 0 10px", fontSize: 15, lineHeight: 1.4 }}>{open.summary}</h3>

          <div className="row" style={{ marginBottom: 14, flexWrap: "wrap" }}>
            <span className="chip chip-type">
              <IssueTypeIcon types={types} name={open.issue_type} size={13} />
              {open.issue_type}
            </span>
            {open.priority && <span className="chip">{open.priority}</span>}
            {open.assignee && <span className="chip">{open.assignee}</span>}
            {open.epic_key && (
              <span className="chip" title={open.epic_summary ?? undefined}>
                epic {open.epic_key}
              </span>
            )}
            {open.components.map((c) => <span key={c} className="chip">{c}</span>)}
            {open.labels.map((l) => <span key={l} className="chip">{l}</span>)}
          </div>

          <Field
            label="Repositories"
            hint="One worktree per repo, on the same branch, side by side in one task folder. You can add more later."
          >
            <RepoPicker
              projects={projects}
              picked={picked}
              onChange={setPicked}
              reason={reason}
            />
          </Field>

          <Field
            label="Branch"
            hint={`Created in every repo you picked, and names the task folder. Leave the suffix blank for just ${open.key}.`}
          >
            <div className="branch-compose">
              <span className="branch-key">{open.key}</span>
              <span className="branch-dash">-</span>
              <input
                value={suffix}
                onChange={(e) => setSuffix(e.target.value)}
                placeholder="optional suffix"
              />
            </div>
          </Field>

          <Field
            label="Agent"
            hint={
              picked.length > 1
                ? "Starts at the task root, where all the repos are visible as sibling folders, primed with the ticket and the layout."
                : "Starts in the new worktree with the ticket as its opening prompt."
            }
          >
            <select value={agentId} onChange={(e) => setAgentId(e.target.value)}>
              <option value="">No agent — just the worktrees</option>
              {installed.map((a) => <option key={a.id} value={a.id}>{a.name}</option>)}
            </select>
          </Field>

          {transitions.length > 0 && (
            <Field label="Move ticket">
              <div className="row" style={{ flexWrap: "wrap" }}>
                {transitions.map((t) => (
                  <button key={t.id} className="btn btn-sm" onClick={() => void doTransition(t)}>
                    {t.name}
                  </button>
                ))}
              </div>
            </Field>
          )}

          {open.description.trim() && (
            <Field label="Description">
              <div
                className="muted"
                style={{
                  whiteSpace: "pre-wrap",
                  maxHeight: 220,
                  overflowY: "auto",
                  border: "1px solid var(--border)",
                  borderRadius: 6,
                  padding: 10,
                  userSelect: "text",
                }}
              >
                {open.description}
              </div>
            </Field>
          )}
        </Modal>
      )}
    </div>
  );
}
