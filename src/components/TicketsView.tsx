import { useEffect, useMemo, useState } from "react";
import { api } from "../lib/api";
import { groupByEpic, useStore } from "../store";
import type { JiraIssue, JiraTransition } from "../lib/types";
import { Field, Modal, Spinner } from "./ui";
import { RepoPicker } from "./RepoPicker";
import { IssueTypeIcon, hierarchyAccent, isEpicType, typeMap } from "./IssueType";

function statusClass(category: string) {
  if (category === "done") return "done";
  if (category === "indeterminate") return "indeterminate";
  return "new";
}

export function TicketsView() {
  const { issues, issueTypes, issuesLoading, settings, projects, agents, tasks } = useStore();
  const refreshIssues = useStore((s) => s.refreshIssues);
  const refreshTasks = useStore((s) => s.refreshTasks);
  const refreshPanes = useStore((s) => s.refreshPanes);
  const toggleSettings = useStore((s) => s.toggleSettings);
  const select = useStore((s) => s.select);
  const toast = useStore((s) => s.toast);
  const fail = useStore((s) => s.fail);

  const [open, setOpen] = useState<JiraIssue | null>(null);
  const [transitions, setTransitions] = useState<JiraTransition[]>([]);
  const [picked, setPicked] = useState<string[]>([]);
  const [reason, setReason] = useState<string | null>(null);
  const [agentId, setAgentId] = useState("");
  const [suffix, setSuffix] = useState("");
  const [starting, setStarting] = useState(false);
  const [shut, setShut] = useState<Record<string, boolean>>({});
  const [query, setQuery] = useState("");

  const installed = agents.filter((a) => a.installed);
  const types = useMemo(() => typeMap(issueTypes), [issueTypes]);

  useEffect(() => {
    if (!agentId && installed.length) setAgentId(installed[0].id);
  }, [installed, agentId]);

  useEffect(() => {
    if (!open) { setTransitions([]); setReason(null); return; }
    setSuffix("");
    api.jiraTransitions(open.key).then(setTransitions).catch(() => setTransitions([]));
    api.suggestRepos({
      issueKey: open.key,
      epicKey: open.epic_key,
      components: open.components,
      labels: open.labels,
    })
      .then((s) => { setPicked(s.project_ids); setReason(s.reason); })
      .catch(() => { setPicked([]); setReason(null); });
  }, [open]);

  async function startWork() {
    if (!open || picked.length === 0) return;
    setStarting(true);
    try {
      const task = await api.jiraStartWork(open.key, picked, agentId || null, suffix || null);
      await Promise.all([refreshTasks(), refreshPanes()]);
      select(task.id);
      setOpen(null);
      toast(
        "success",
        `${picked.length} worktree${picked.length === 1 ? "" : "s"} ready on ${task.branch}`,
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

  const q = query.trim().toLowerCase();
  const filtered = q
    ? issues.filter(
        (i) =>
          i.key.toLowerCase().includes(q) ||
          i.summary.toLowerCase().includes(q) ||
          i.labels.some((l) => l.toLowerCase().includes(q)) ||
          i.components.some((c) => c.toLowerCase().includes(q)),
      )
    : issues;
  const epics = groupByEpic(filtered);
  const started = (key: string) => tasks.some((t) => t.issue_key === key);

  return (
    <div className="wide">
      <div className="wide-head">
        <h2>Tickets</h2>
        <span className="sub">
          {filtered.length} issue{filtered.length === 1 ? "" : "s"} in {epics.length} epic
          {epics.length === 1 ? "" : "s"}
        </span>
        {issuesLoading && <Spinner />}
        <div className="spacer" />
        <input
          type="text"
          style={{ width: 220 }}
          placeholder="Filter by key, title, label…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <button className="btn btn-sm" onClick={() => void refreshIssues()}>Refresh</button>
      </div>

      {epics.length === 0 && !issuesLoading && (
        <div className="card">
          <div className="muted">
            {q ? `Nothing matches “${query}”.` : "No issues matched your JQL. Adjust it in Settings."}
          </div>
        </div>
      )}

      {epics.map((epic) => {
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
            >
              <span className={`chev${closed ? "" : " open"}`}>▶</span>
              {epic.key && <IssueTypeIcon types={types} name={epicTypeName} size={18} />}
              {epic.key ? <span className="key-chip">{epic.key}</span> : null}
              <span className="title">{epic.summary || (epic.key ? epic.key : "No epic")}</span>
              <span className="count">{epic.issues.length}</span>
              <div className="spacer" />
              {/* The epic is only openable when it is assigned to you too. */}
              {epic.issue && (
                <button
                  className="btn btn-sm"
                  onClick={(e) => { e.stopPropagation(); setOpen(epic.issue!); }}
                >
                  Open epic
                </button>
              )}
            </div>

            {!closed && (
              <div className="epic-body">
                {epic.issues.map((issue) => (
                  <div
                    key={issue.key}
                    className={
                      `ticket${started(issue.key) ? " started" : ""}` +
                      (isEpicType(types, issue.issue_type) ? " is-epic" : "")
                    }
                    style={{ borderLeftColor: hierarchyAccent(types, issue.issue_type) }}
                    onClick={() => setOpen(issue)}
                  >
                    <div className="top">
                      <IssueTypeIcon types={types} name={issue.issue_type} />
                      <span className="key-chip">{issue.key}</span>
                      <span className={`status-pill ${statusClass(issue.status_category)}`}>
                        {issue.status}
                      </span>
                      {started(issue.key) && <span className="chip add">started</span>}
                    </div>
                    <div className="summary">{issue.summary}</div>
                    <div className="bottom">
                      <span>{issue.issue_type}</span>
                      {issue.priority && <span>· {issue.priority}</span>}
                      {issue.components.map((c) => (
                        <span key={c} className="group-chip">{c}</span>
                      ))}
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
        );
      })}

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
