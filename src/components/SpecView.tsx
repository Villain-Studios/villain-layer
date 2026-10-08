import { useEffect, useState } from "react";
import { api } from "../lib/api";
import { approvedText, editKey, editorText, useSpecs } from "../lib/specs";
import { useStore } from "../store";
import type { RepoSpec, SpecKind, SpecPart, TaskView } from "../lib/types";
import { Confirm, Spinner } from "./ui";
import { SpecSide } from "./spec/SpecSide";

const PARTS: SpecPart[] = ["requirements", "design", "tasks"];

/** What "Write them yourself" starts from: the headings a draft has (SPEC-1). */
const TEMPLATE: Record<SpecKind, string> = {
  feature: "## Goal\n\n## Requirements\n\n- R-1: WHEN … THE SYSTEM SHALL …\n\n## Out of scope\n\n-\n\n## Open questions\n\nNone.\n",
  bugfix: "## Current behaviour\n\n## Expected behaviour\n\n- R-1: WHEN … THE SYSTEM SHALL …\n\n## Unchanged behaviour\n\n- R-2: WHEN … THE SYSTEM SHALL CONTINUE TO …\n\n## Open questions\n\nNone.\n",
};

export function partName(part: SpecPart, kind: SpecKind): string {
  if (part === "requirements") return kind === "bugfix" ? "Bug analysis" : "Requirements";
  return part === "design" ? "Design" : "Tasks";
}

/**
 * A task's specs (§19): for each repository, requirements, design and tasks,
 * each drafted, edited here and approved in turn. Approving commits the file
 * on the task's branch, so it goes to review with the code and stays in the
 * repository after the task is gone.
 */
export function SpecView({ task }: { task: TaskView }) {
  const state = useSpecs((s) => s.byTask[task.id]);
  const load = useSpecs((s) => s.load);
  const edit = useSpecs((s) => s.edit);
  const draft = useSpecs((s) => s.draft);
  const approve = useSpecs((s) => s.approve);
  const pending = useSpecs((s) => s.pending);
  const setPending = useSpecs((s) => s.setPending);
  const running = useStore((s) => s.panes.filter((p) => p.task_id === task.id && p.running && p.kind === "agent").length);
  const fail = useStore((s) => s.fail);
  const toast = useStore((s) => s.toast);

  const [checkoutId, setCheckoutId] = useState<string | null>(null);
  const [part, setPart] = useState<SpecPart | null>(null);
  const [kind, setKind] = useState<SpecKind>("feature");
  const [agentId, setAgentId] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [replacing, setReplacing] = useState(false);
  /** What was just approved while agents were running, to tell them (SPEC-14). */
  const [tell, setTell] = useState<{ checkoutId: string; parts: SpecPart[] } | null>(null);

  const loaded = state?.loaded ?? false;
  useEffect(() => {
    if (!loaded) void load(task.id).catch(fail);
  }, [task.id, loaded, load, fail]);
  // Feature or bug, as the spec already says once there is one.
  const savedKind = state?.spec?.repos[0]?.kind;
  useEffect(() => {
    if (savedKind) setKind(savedKind);
  }, [task.id, savedKind]);

  const repos = state?.spec?.repos ?? [];
  const repo = repos.find((r) => r.checkout_id === checkoutId) ?? repos[0];
  const edits = state?.edits ?? {};
  const started = repos.some((r) => r.parts.some((p) => p.approved)) || Object.keys(edits).length > 0;
  const drafting = state?.drafting ?? null;

  // Start work's choice (SPEC-18): the agent to start later, and the
  // requirements drafted now, unless there is a spec already.
  useEffect(() => {
    if (!pending || pending.taskId !== task.id || !loaded) return;
    setPending(null);
    setAgentId(pending.agentId);
    if (!started && !drafting) void draft(task.id, "requirements", null, null).catch(fail);
  }, [pending, task.id, loaded, started, drafting, setPending, draft, fail]);

  if (!loaded) return <div className="empty"><Spinner /></div>;
  if (!repo) {
    return (
      <div className="empty">
        <h2>No repositories</h2>
        <p>A spec is kept in each repository the task works in. Add one to the task first.</p>
      </div>
    );
  }

  if (!started && !drafting) {
    return (
      <SpecStart
        kind={kind}
        setKind={setKind}
        from={task.issue_key ? "the ticket" : "the task's name"}
        legacy={state?.spec?.legacy ?? null}
        busy={busy}
        onDraft={() => void draft(task.id, "requirements", null, kind).catch(fail)}
        onQuick={() => void quick()}
        onWrite={(text) => repos.forEach((r) => edit(task.id, r.checkout_id, "requirements", text))}
      />
    );
  }

  const current = part ?? firstOpen(repo);
  const view = repo.parts.find((p) => p.part === current)!;
  const key = editKey(repo.checkout_id, current);
  const text = editorText(state, repo, current);
  const dirty = edits[key] !== undefined;
  const before = PARTS[PARTS.indexOf(current) - 1];
  const ready = !before || !!repo.parts.find((p) => p.part === before)?.approved || edits[editKey(repo.checkout_id, before)] !== undefined;
  const isDrafting = drafting?.part === current;
  const allDrafted = repo.parts.every((p) => !p.approved && edits[editKey(repo.checkout_id, p.part)] !== undefined);

  async function quick() {
    setBusy(true);
    try {
      await draft(task.id, "requirements", null, kind);
      await draft(task.id, "design", null, null);
      await draft(task.id, "tasks", null, null);
      setPart("requirements");
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  async function approveParts(parts: SpecPart[]) {
    if (!repo) return;
    setBusy(true);
    try {
      await approve(task.id, repo.checkout_id, parts, parts.includes("requirements") ? kind : null);
      toast("success", repo.in_repo ? `Approved and committed in ${repo.folder}.` : "Approved.");
      if (running > 0) setTell({ checkoutId: repo.checkout_id, parts });
      const next = PARTS[PARTS.indexOf(parts[parts.length - 1]) + 1];
      if (next && parts.length === 1) setPart(next);
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  function redraft() {
    if (!repo) return;
    void draft(task.id, current, current === "requirements" ? null : repo.checkout_id, null).catch(fail);
  }

  const status = isDrafting
    ? current === "requirements" && repos.length > 1 ? "Drafting for every repository…" : "Drafting…"
    : drafting
      ? `Drafting the ${drafting.part}…`
      : view.stale
      ? `Out of date: the ${current === "tasks" ? "design or requirements" : "requirements"} changed since. Sync redrafts it.`
      : dirty
        ? view.approved ? "Changed since approved: agents still work to what was approved" : "Not approved: no agent sees it yet"
        : view.approved
          ? repo.in_repo ? "Approved and committed" : "Approved"
          : "Not started";
  const draftLabel = view.stale ? "Sync" : text.trim() ? "Redraft" : "Draft";
  const draftTitle = current === "requirements"
    ? `From ${task.issue_key ? "the ticket" : "the task's name"}, for every repository at once`
    : current === "design" ? `From the requirements, reading ${repo.folder}'s code` : "From the requirements and the design";

  return (
    <div className="spec">
      {repos.length > 1 && (
        <div className="spec-repos">
          {repos.map((r) => (
            <button
              key={r.checkout_id}
              className={`spec-repo${r.checkout_id === repo.checkout_id ? " active" : ""}`}
              onClick={() => { setCheckoutId(r.checkout_id); setPart(null); }}
            >
              {r.folder}
              <span className="muted">{r.parts.filter((p) => p.approved && !p.stale).length}/3</span>
            </button>
          ))}
        </div>
      )}
      <div className="spec-steps">
        {PARTS.map((p, i) => {
          const v = repo.parts.find((x) => x.part === p)!;
          const has = edits[editKey(repo.checkout_id, p)] !== undefined;
          const label = v.stale ? "out of date" : has ? (v.approved ? "changed" : "draft") : v.approved ? "approved" : "not started";
          return (
            <button key={p} className={`spec-step${p === current ? " active" : ""}`} onClick={() => setPart(p)}>
              <span className={`spec-num ${v.approved && !v.stale && !has ? "done" : ""}`}>{v.approved && !v.stale && !has ? "✓" : i + 1}</span>
              <span>{partName(p, repo.kind)}</span>
              <span className={`spec-state ${label.replace(/ /g, "-")}`}>{label}</span>
            </button>
          );
        })}
        <span className="spacer" />
        <span className="muted spec-home" title={repo.in_repo ? "Committed on the task's branch" : "Specs are off for this repository (Repos): kept by the app, not in git"}>
          {repo.in_repo ? repo.home : "kept by the app"}
        </span>
      </div>
      {tell && (
        <div className="spec-tell">
          {running} agent{running === 1 ? " is" : "s are"} working in this task to the spec as it was.
          <button className="btn btn-sm" onClick={() => {
            void api.tellSpecChange(task.id, tell.checkoutId, tell.parts).then((n) => toast("success", `Told ${n} agent${n === 1 ? "" : "s"}.`)).catch(fail);
            setTell(null);
          }}>Tell them</button>
          <button className="btn btn-sm" onClick={() => setTell(null)}>Not now</button>
        </div>
      )}
      <div className="spec-bar">
        <button
          className="btn btn-sm"
          disabled={!!drafting || busy || !ready}
          title={ready ? draftTitle : `Approve the ${before} first`}
          onClick={() => (text.trim() && !view.stale ? setReplacing(true) : redraft())}
        >
          {draftLabel} {current === "requirements" ? "" : current}
        </button>
        {(isDrafting || busy) && <Spinner />}
        <span className="muted">{status}</span>
        <span className="spacer" />
        {dirty && (
          <button
            className="btn btn-sm"
            disabled={!!drafting || busy}
            onClick={() => edit(task.id, repo.checkout_id, current, approvedText(repo, current))}
          >
            {view.approved ? "Discard changes" : "Discard draft"}
          </button>
        )}
        {allDrafted && (
          <button className="btn btn-sm" disabled={!!drafting || busy} onClick={() => void approveParts(PARTS)}>
            Approve all three
          </button>
        )}
        <button
          className="btn btn-sm btn-primary"
          disabled={!!drafting || busy || !(dirty || (view.stale && text.trim()))}
          title={repo.in_repo ? `Write it to ${repo.home} and commit it on the task's branch` : "Keep it as the approved spec"}
          onClick={() => void approveParts([current])}
        >
          Approve {partName(current, repo.kind).toLowerCase()}
        </button>
      </div>
      <div className="spec-body">
        <textarea
          className="spec-editor"
          value={text}
          readOnly={isDrafting}
          spellCheck={false}
          placeholder={current === "tasks" ? "## Tasks\n\n- [ ] 1. … (R-1)" : "## …"}
          onChange={(e) => edit(task.id, repo.checkout_id, current, e.target.value)}
        />
        <SpecSide task={task} repo={repo} repos={repos} agentId={agentId} setAgentId={setAgentId} />
      </div>
      {replacing && (
        <Confirm
          title="Replace what the editor holds?"
          body={current === "requirements" && repos.length > 1
            ? "A new draft replaces the requirements of every repository in the editor. What is approved stays until you approve again."
            : "A new draft replaces what is in the editor. What is approved stays until you approve again."}
          confirmLabel="Redraft"
          danger={false}
          onConfirm={() => { setReplacing(false); redraft(); }}
          onCancel={() => setReplacing(false)}
        />
      )}
    </div>
  );
}

/** The first file not yet approved, or the tasks. */
function firstOpen(repo: RepoSpec): SpecPart {
  return repo.parts.find((p) => !p.approved || p.stale)?.part ?? "tasks";
}

/** Before any spec: what it is, a bug or a feature, and how to start one. */
function SpecStart({ kind, setKind, from, legacy, busy, onDraft, onQuick, onWrite }: {
  kind: SpecKind;
  setKind: (k: SpecKind) => void;
  from: string;
  legacy: string | null;
  busy: boolean;
  onDraft: () => void;
  onQuick: () => void;
  onWrite: (text: string) => void;
}) {
  return (
    <div className="empty spec-start">
      <h2>No spec yet</h2>
      <p>
        A spec is three files in each repository, reviewed in turn: the requirements (what done
        means), the design (how), and the tasks an agent works through. Approving one commits it on
        the task's branch, so it goes to review with the code and stays in the repository.
      </p>
      <div className="row spec-kind">
        <label><input type="radio" checked={kind === "feature"} onChange={() => setKind("feature")} /> A feature</label>
        <label><input type="radio" checked={kind === "bugfix"} onChange={() => setKind("bugfix")} /> A bug</label>
      </div>
      <div className="row">
        <button className="btn btn-primary" disabled={busy} onClick={onDraft}>Draft the {kind === "bugfix" ? "bug analysis" : "requirements"} from {from}</button>
        <button className="btn" disabled={busy} onClick={onQuick} title="Requirements, design and tasks in one go, to approve together">Quick spec</button>
        <button className="btn" disabled={busy} onClick={() => onWrite(TEMPLATE[kind])}>Write it yourself</button>
      </div>
      {legacy && (
        <p className="muted">
          This task has a spec from before, kept beside the task.{" "}
          <button className="btn btn-sm" onClick={() => onWrite(legacy)}>Start from it</button>
        </p>
      )}
    </div>
  );
}
