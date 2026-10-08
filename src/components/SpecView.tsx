import { useEffect, useState } from "react";
import { api } from "../lib/api";
import { useStore } from "../store";
import type { TaskView } from "../lib/types";
import { Confirm, Spinner } from "./ui";

/** What "Write one yourself" starts from: the headings a draft has (SPEC-1). */
const TEMPLATE = `## Goal

## Acceptance criteria

- [ ] AC-1:

## Out of scope

-

## Open questions

None.
`;

/**
 * A task's spec (§19): drafted from the ticket, edited here, and only once
 * saved given to the task's agents. Start agent saves it and starts one.
 */
export function SpecView({ task }: { task: TaskView }) {
  const spec = useStore((s) => s.specs[task.id]);
  const pending = useStore((s) => s.pendingSpec);
  const agents = useStore((s) => s.agents);
  const loadSpec = useStore((s) => s.loadSpec);
  const editSpec = useStore((s) => s.editSpec);
  const saveSpec = useStore((s) => s.saveSpec);
  const draftSpec = useStore((s) => s.draftSpec);
  const setPendingSpec = useStore((s) => s.setPendingSpec);
  const refreshPanes = useStore((s) => s.refreshPanes);
  const showPane = useStore((s) => s.showPane);
  const setTab = useStore((s) => s.setTab);
  const toast = useStore((s) => s.toast);
  const fail = useStore((s) => s.fail);

  const installed = agents.filter((a) => a.installed);
  const [agentId, setAgentId] = useState(installed[0]?.id ?? "");
  const [busy, setBusy] = useState(false);
  const [replacing, setReplacing] = useState(false);

  const loaded = spec?.loaded ?? false;
  useEffect(() => {
    if (!loaded) void loadSpec(task.id).catch(fail);
  }, [task.id, loaded, loadSpec, fail]);

  // A default agent once the list arrives, unless one was picked.
  const firstInstalled = installed[0]?.id;
  useEffect(() => {
    if (!agentId && firstInstalled) setAgentId(firstInstalled);
  }, [agentId, firstInstalled]);

  // Start work's choice (SPEC-6): the agent to start later, and a draft now,
  // unless the task already has a spec.
  useEffect(() => {
    if (!pending || pending.taskId !== task.id || !spec?.loaded) return;
    setPendingSpec(null);
    if (pending.agentId) setAgentId(pending.agentId);
    if (!spec.saved && spec.edit === null && !spec.drafting) void draftSpec(task.id);
  }, [pending, task.id, spec, setPendingSpec, draftSpec]);

  if (!spec?.loaded) {
    return <div className="empty"><Spinner /></div>;
  }

  const drafting = spec.drafting !== null;
  const text = spec.edit ?? spec.saved?.text ?? "";
  const dirty = spec.edit !== null;
  const from = task.issue_key ? "the ticket" : "the task's name";

  if (!spec.saved && !dirty && !drafting) {
    return (
      <div className="empty">
        <h2>No spec yet</h2>
        <p>
          A spec says what done means: the goal, the acceptance criteria, and what is out of
          scope. Every agent in this task works to it once you save it.
        </p>
        <div className="row">
          <button className="btn btn-primary" onClick={() => void draftSpec(task.id)}>
            Draft from {from}
          </button>
          <button className="btn" onClick={() => editSpec(task.id, TEMPLATE)}>
            Write one yourself
          </button>
        </div>
      </div>
    );
  }

  async function save() {
    setBusy(true);
    try {
      await saveSpec(task.id);
      toast("success", text.trim() ? "Spec saved. Agents in this task now work to it." : "Spec removed.");
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  async function startAgent() {
    if (!agentId) return;
    setBusy(true);
    try {
      if (dirty || !spec?.saved) await saveSpec(task.id);
      const prompt = await api.taskPrompt(task.id, null);
      const pane = await api.spawnAgent(task.id, agentId, null, prompt);
      await refreshPanes();
      showPane(pane.id);
      setTab("terminals");
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  const criteria = spec.saved?.criteria ?? [];
  const status = drafting
    ? `Drafting from ${from}…`
    : dirty
      ? spec.saved ? "Changed since saved: agents still see the saved spec" : "Not saved: agents do not see it yet"
      : `Saved · ${criteria.length} acceptance criteri${criteria.length === 1 ? "on" : "a"}`;

  return (
    <div className="spec">
      <div className="spec-bar">
        <button
          className="btn btn-sm"
          disabled={drafting || busy}
          onClick={() => (text.trim() ? setReplacing(true) : void draftSpec(task.id))}
        >
          {text.trim() ? "Redraft" : "Draft"} from {from}
        </button>
        {drafting && <Spinner />}
        <span className="muted">{status}</span>
        <span style={{ flex: 1 }} />
        {dirty && spec.saved && (
          <button
            className="btn btn-sm"
            disabled={drafting || busy}
            onClick={() => editSpec(task.id, spec.saved?.text ?? "")}
          >
            Discard changes
          </button>
        )}
        <button className="btn btn-sm" disabled={!dirty || drafting || busy} onClick={() => void save()}>
          Save
        </button>
        <select
          value={agentId}
          disabled={installed.length === 0}
          onChange={(e) => setAgentId(e.target.value)}
          title="The agent Start agent starts, at the task folder"
        >
          {installed.length === 0 && <option value="">No agent CLI installed</option>}
          {installed.map((a) => <option key={a.id} value={a.id}>{a.name}</option>)}
        </select>
        <button
          className="btn btn-sm btn-primary"
          disabled={!agentId || !text.trim() || drafting || busy}
          title="Save the spec, then start the agent with it"
          onClick={() => void startAgent()}
        >
          {busy ? "Starting…" : "Start agent"}
        </button>
      </div>
      {spec.saved && !dirty && criteria.length === 0 && (
        <div className="spec-warn">
          No acceptance criteria found. List them under an “Acceptance criteria” heading:
          they are what done means.
        </div>
      )}
      <div className="spec-body">
        <textarea
          className="spec-editor"
          value={text}
          readOnly={drafting}
          spellCheck={false}
          placeholder="## Goal…"
          onChange={(e) => editSpec(task.id, e.target.value)}
        />
        {criteria.length > 0 && (
          <aside className="spec-criteria">
            <h3>Acceptance criteria</h3>
            <ol>
              {criteria.map((c) => (
                <li key={c.id}><b>{c.id}</b> {c.text}</li>
              ))}
            </ol>
          </aside>
        )}
      </div>
      {replacing && (
        <Confirm
          title="Replace the spec in the editor?"
          body={`A new draft from ${from} replaces what is in the editor. What is saved stays until you save again.`}
          confirmLabel="Redraft"
          danger={false}
          onConfirm={() => { void draftSpec(task.id); }}
          onCancel={() => setReplacing(false)}
        />
      )}
    </div>
  );
}
