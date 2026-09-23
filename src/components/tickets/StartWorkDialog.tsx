import { useEffect, useRef, useState } from "react";
import { api } from "../../lib/api";
import { useStore } from "../../store";
import type { AgentStatus, JiraIssue, JiraTransition, Project } from "../../lib/types";
import { Combo, Field, Modal } from "../ui";
import { RepoPicker } from "../RepoPicker";
import { IssueTypeIcon, type TypeMap } from "../IssueType";

export function StartWorkDialog({
  issue,
  projects,
  agents,
  types,
  onClose,
}: {
  issue: JiraIssue;
  projects: Project[];
  agents: AgentStatus[];
  types: TypeMap;
  onClose: () => void;
}) {
  const refreshIssues = useStore((s) => s.refreshIssues);
  const refreshTasks = useStore((s) => s.refreshTasks);
  const refreshPanes = useStore((s) => s.refreshPanes);
  const select = useStore((s) => s.select);
  const toast = useStore((s) => s.toast);
  const fail = useStore((s) => s.fail);

  const [transitions, setTransitions] = useState<JiraTransition[]>([]);
  const [picked, setPicked] = useState<string[]>([]);
  const [reason, setReason] = useState<string | null>(null);
  /** Null until a default is picked; "" is the choice of no agent at all. */
  const [agentId, setAgentId] = useState<string | null>(null);
  const [suffix, setSuffix] = useState("");
  const [base, setBase] = useState("");
  const [baseOptions, setBaseOptions] = useState<string[]>([]);
  /** The base this dialog last filled in itself, as opposed to one typed. */
  const autoBase = useRef("");
  const [starting, setStarting] = useState(false);

  const installed = agents.filter((a) => a.installed);
  const selected = projects.filter((p) => picked.includes(p.id));

  // A default, once. Re-filled whenever the value was empty — and on every
  // render, since `installed` is a new list each time — this put the first
  // agent back the moment "No agent" was picked, and Start work launched one.
  const firstInstalled = installed[0]?.id;
  useEffect(() => {
    if (agentId === null && firstInstalled) setAgentId(firstInstalled);
  }, [firstInstalled, agentId]);

  useEffect(() => {
    setTransitions([]);
    setReason(null);
    setSuffix("");
    setBase("");
    setBaseOptions([]);
    setPicked([]);
    let stop = false;
    api.jiraTransitions(issue.key)
      .then((t) => { if (!stop) setTransitions(t); })
      .catch(() => { if (!stop) setTransitions([]); });
    // A suggestion, so it gives way to what was picked while it was out.
    api.suggestRepos({ issueKey: issue.key, epicKey: issue.epic_key })
      .then((s) => {
        if (stop) return;
        setPicked((p) => (p.length > 0 ? p : s.project_ids));
        setReason(s.reason);
      })
      .catch(() => { if (!stop) setReason(null); });
    return () => { stop = true; };
  }, [issue]);

  // Prefill from the repos' defaults when they agree; load the union of their
  // remote-tracking branches for the picker. Typing still works — a branch
  // pushed since the last fetch will not be in the list.
  useEffect(() => {
    if (selected.length === 0) {
      setBase("");
      setBaseOptions([]);
      return;
    }
    const defaults = [...new Set(selected.map((p) => p.default_branch))];
    // Suggest, do not overwrite: a base typed by hand survives adding another
    // repository. Only a box still showing the last suggestion follows it.
    const suggested = defaults.length === 1 ? defaults[0] : "";
    setBase((cur) => (cur === "" || cur === autoBase.current ? suggested : cur));
    autoBase.current = suggested;
    let cancelled = false;
    Promise.all(selected.map((p) => api.projectBranches(p.id).catch(() => [] as string[])))
      .then((lists) => {
        if (cancelled) return;
        const seen = new Set<string>();
        const merged: string[] = [];
        for (const list of lists) {
          for (const b of list) {
            if (!seen.has(b)) {
              seen.add(b);
              merged.push(b);
            }
          }
        }
        // Keep each repo's default near the top even when it is not the newest.
        for (const d of defaults) {
          if (!seen.has(d)) merged.unshift(d);
        }
        setBaseOptions(merged);
      });
    return () => { cancelled = true; };
  }, [picked.join(","), projects]);

  async function startWork() {
    if (picked.length === 0) return;
    setStarting(true);
    try {
      const task = await api.jiraStartWork(
        issue.key,
        picked,
        agentId || null,
        suffix || null,
        base.trim() || null,
      );
      // Refresh the issues too: the ticket has usually just moved, and the
      // status on the card is the thing the move was meant to correct.
      await Promise.all([refreshTasks(), refreshPanes(), refreshIssues()]);
      select(task.id);
      onClose();
      toast(
        "success",
        `${picked.length} worktree${picked.length === 1 ? "" : "s"} ready on ${task.branch}` +
          (task.moved ? ` · ${issue.key} → ${task.moved}` : ""),
      );
    } catch (e) {
      fail(e);
    } finally {
      setStarting(false);
    }
  }

  async function doTransition(t: JiraTransition) {
    try {
      await api.jiraTransition(issue.key, t.id);
      toast("success", `${issue.key} → ${t.to_status}`);
      onClose();
      await refreshIssues();
    } catch (e) {
      fail(e);
    }
  }

  const baseHint = selected.length === 0
    ? "Pick repositories first."
    : selected.length === 1
      ? `New branch starts at this tip in ${selected[0].name}.`
      : "Created from this tip in every repo you picked. Blank uses each repo's own default.";

  return (
    <Modal
      title={`${issue.key} · ${issue.status}`}
      wide
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>Close</button>
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
      <h3 style={{ margin: "0 0 10px", fontSize: 15, lineHeight: 1.4 }}>{issue.summary}</h3>

      <div className="row" style={{ marginBottom: 14, flexWrap: "wrap" }}>
        <span className="chip chip-type">
          <IssueTypeIcon types={types} name={issue.issue_type} size={13} />
          {issue.issue_type}
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
        hint={`Created in every repo you picked, and names the task folder. Leave the suffix blank for just ${issue.key}.`}
      >
        <div className="branch-compose">
          <span className="branch-key">{issue.key}</span>
          <span className="branch-dash">-</span>
          <input
            value={suffix}
            onChange={(e) => setSuffix(e.target.value)}
            placeholder="optional suffix"
          />
        </div>
      </Field>

      <Field label="Branch from" hint={baseHint}>
        <Combo
          value={base}
          options={baseOptions}
          placeholder={
            selected.length > 1 && !base
              ? "(each repo's default)"
              : selected[0]?.default_branch ?? "main"
          }
          empty="No branch matches"
          onChange={setBase}
        />
      </Field>

      <Field
        label="Agent"
        hint={
          picked.length > 1
            ? "Starts at the task root, where all the repos are visible as sibling folders, primed with the ticket and the layout."
            : "Starts in the new worktree with the ticket as its opening prompt."
        }
      >
        <select value={agentId ?? ""} onChange={(e) => setAgentId(e.target.value)}>
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

      {issue.description.trim() && (
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
            {issue.description}
          </div>
        </Field>
      )}
    </Modal>
  );
}
