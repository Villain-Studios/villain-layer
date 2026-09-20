import { useEffect, useMemo, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api } from "../../lib/api";
import { groupByEpic, useStore } from "../../store";
import type { JiraIssue, JiraPage } from "../../lib/types";
import { ContextMenu, Spinner, type MenuItem } from "../ui";
import { IssueTypeIcon, isEpicType, typeMap } from "../IssueType";
import { BrowsePanel } from "./BrowsePanel";
import { FileIssueDialog } from "./FileIssueDialog";
import { ReadTicketDialog } from "./ReadTicketDialog";
import { StartWorkDialog } from "./StartWorkDialog";
import { TicketCard } from "./TicketCard";
import { TypeFilter } from "./TypeFilter";

export function TicketsView() {
  const issues = useStore((s) => s.issues);
  const issueTypes = useStore((s) => s.issueTypes);
  const issuesLoading = useStore((s) => s.issuesLoading);
  const issuesTruncated = useStore((s) => s.issuesTruncated);
  const settings = useStore((s) => s.settings);
  const projects = useStore((s) => s.projects);
  const agents = useStore((s) => s.agents);
  const tasks = useStore((s) => s.tasks);
  const refreshIssues = useStore((s) => s.refreshIssues);
  const toggleSettings = useStore((s) => s.toggleSettings);
  const select = useStore((s) => s.select);
  const toast = useStore((s) => s.toast);
  const fail = useStore((s) => s.fail);

  const [open, setOpen] = useState<JiraIssue | null>(null);
  // Reading a ticket is not starting one. Jira already sent the description
  // with the rest of the issue, so this costs nothing but a dialog.
  const [reading, setReading] = useState<JiraIssue | null>(null);
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

  const jiraBase = settings?.jira?.base_url.replace(/\/+$/, "") ?? "";
  const types = useMemo(() => typeMap(issueTypes), [issueTypes]);

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
        onSelect: () => setFiling({ key, summary }),
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
        onSelect: () => (task ? select(task) : setOpen(issue)),
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

  if (!settings?.jira_connected) {
    return (
      <div className="empty">
        <h2>Jira not connected</h2>
        <p>Connect Jira to pull your assigned issues and start work from a ticket.</p>
        <button className="btn" onClick={() => toggleSettings(true)}>Connect Jira</button>
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
  const taskFor = (key: string) => tasks.find((t) => t.issue_key === key)?.id ?? null;
  const started = new Set(
    tasks.map((t) => t.issue_key).filter((k): k is string => !!k),
  );
  const epics = groupByEpic(filtered, started);
  // Collapsed is stored per epic, so "all" means every one currently listed —
  // which is what a filter has narrowed things to, not the whole board.
  const allShut = epics.length > 0 && epics.every((e) => shut[e.key] ?? false);

  function renderCard(issue: JiraIssue) {
    return (
      <TicketCard
        key={issue.key}
        issue={issue}
        types={types}
        taskId={taskFor(issue.key)}
        syncing={syncing === issue.key}
        onOpen={() => setOpen(issue)}
        onSelectTask={select}
        onSync={() => void syncStatus(issue.key)}
        onFileUnder={() => setFiling({ key: issue.key, summary: issue.summary })}
        onContextMenu={(e) => {
          e.preventDefault();
          setMenu({ x: e.clientX, y: e.clientY, items: issueMenu(issue) });
        }}
      />
    );
  }

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
            className={sub === "browse" ? "active" : ""}
            onClick={() => setSub("browse")}
            title="Search the whole board, not just what is assigned to you"
          >
            Find work
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
        <BrowsePanel
          issueTypes={issueTypes}
          types={types}
          kinds={kinds}
          onKinds={setKinds}
          browseText={browseText}
          onBrowseText={setBrowseText}
          whose={whose}
          onWhose={setWhose}
          includeDone={includeDone}
          onIncludeDone={setIncludeDone}
          searching={searching}
          found={found}
          onSearch={() => void browse()}
          taskFor={taskFor}
          syncing={syncing}
          onOpen={setOpen}
          onSelectTask={select}
          onSync={(key) => void syncStatus(key)}
          onFileUnder={setFiling}
          onContextMenu={(e, issue) => {
            setMenu({ x: e.clientX, y: e.clientY, items: issueMenu(issue) });
          }}
        />
      )}

      {sub === "mine" && (
        <TypeFilter
          issueTypes={issueTypes}
          types={types}
          kinds={kinds}
          onChange={setKinds}
        />
      )}

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
                          onSelect: () => {
                            const id = taskFor(own.key);
                            if (id) select(id);
                            else setOpen(own);
                          },
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
                    const id = taskFor(epic.issue!.key);
                    if (id) select(id);
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
                {epic.issues.map(renderCard)}
              </div>
            )}
          </div>
        );
      })}

      {filing && (
        <FileIssueDialog
          epic={filing}
          issueTypes={issueTypes}
          types={types}
          onClose={() => setFiling(null)}
          onFiled={(issue) => setOpen(issue)}
        />
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
        <ReadTicketDialog
          issue={reading}
          types={types}
          hasTask={!!taskFor(reading.key)}
          onClose={() => setReading(null)}
          onStart={() => { setOpen(reading); setReading(null); }}
        />
      )}

      {open && (
        <StartWorkDialog
          issue={open}
          projects={projects}
          agents={agents}
          types={types}
          onClose={() => setOpen(null)}
        />
      )}
    </div>
  );
}
