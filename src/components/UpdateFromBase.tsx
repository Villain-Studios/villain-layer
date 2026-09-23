import { useState } from "react";
import { api } from "../lib/api";
import { useStore } from "../store";
import type { RepoUpdate, TaskView, UpdateBy } from "../lib/types";
import { Modal, Spinner } from "./ui";
import { AgentTargetFields, useAgentTarget } from "./AgentTarget";

const OUTCOME_DOT: Record<RepoUpdate["outcome"], string> = {
  up_to_date: "var(--dimmer)",
  updated: "var(--green)",
  conflicts: "var(--red)",
  failed: "var(--amber)",
};

/** What each way does, said where the choice is made. */
const HOW: Record<UpdateBy, string> = {
  merge:
    "Merges each base into the branch, the way GitHub's Update branch does. History is kept, so an open pull request needs no force push.",
  rebase:
    "Replays the branch's commits on top of each base, for a straight history. That rewrites what was pushed: the next push replaces the remote branch — but only if it is still what was rebased from, so nobody else's push is lost.",
};

/**
 * Bring each repository's base branch into the task branch, and deal with
 * what does not go in cleanly.
 *
 * A conflict is left in progress and handed to an agent, because that is the
 * state it can be resolved from — or abandoned, which puts the branch back.
 */
export function UpdateFromBase({ task, onClose }: { task: TaskView; onClose: () => void }) {
  const refreshTasks = useStore((s) => s.refreshTasks);
  const refreshSettings = useStore((s) => s.refreshSettings);
  const lastBy = useStore((s) => s.settings?.update_by ?? "merge");
  const toast = useStore((s) => s.toast);
  const fail = useStore((s) => s.fail);
  const at = useAgentTarget(task);

  const [picked, setPicked] = useState<Set<string>>(
    () => new Set(task.checkouts.filter((c) => c.exists).map((c) => c.id)),
  );
  const [results, setResults] = useState<RepoUpdate[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [aborted, setAborted] = useState<Set<string>>(new Set());
  // The last one used: a team that rebases rebases every time.
  const [by, setBy] = useState<UpdateBy>(lastBy);
  /** What the results were produced by, for wording them. */
  const [ran, setRan] = useState<UpdateBy | null>(null);

  // Unfinished merges: from this run, or already there when the dialog
  // opened — an agent may not have finished the last one.
  const conflicted = task.checkouts.filter(
    (c) =>
      !aborted.has(c.id) &&
      ((c.status?.conflicted ?? 0) > 0 ||
        results?.some((r) => r.checkout_id === c.id && r.outcome === "conflicts")),
  );

  async function update() {
    setBusy(true);
    try {
      const out = await api.updateFromBase(task.id, [...picked], by);
      setResults(out);
      setRan(by);
      setAborted(new Set());
      const updated = out.filter((r) => r.outcome === "updated").length;
      const clashes = out.filter((r) => r.outcome === "conflicts").length;
      if (updated > 0 && clashes === 0) {
        toast(
          "success",
          `${by === "rebase" ? "Rebased" : "Brought"} ${updated} repo${updated === 1 ? "" : "s"} up to date` +
            (by === "rebase" ? " — Push all replaces the remote branches" : ""),
        );
      }
      await Promise.all([refreshTasks().catch(() => {}), refreshSettings().catch(() => {})]);
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  async function abort(checkoutId: string) {
    setBusy(true);
    try {
      await api.abortUpdate(checkoutId);
      setAborted((a) => new Set(a).add(checkoutId));
      await refreshTasks().catch(() => {});
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  async function handOff() {
    setBusy(true);
    try {
      const who = await at.send((paneId, scope) => api.sendMergeConflicts(task.id, paneId, scope));
      toast("success", `Handed the conflicts to ${who}.`);
      onClose();
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  const running = at.running.length;
  const footer = results === null && conflicted.length === 0 ? (
    <>
      <div className="spacer" />
      <button className="btn" onClick={onClose} disabled={busy}>Cancel</button>
      <button
        className="btn btn-primary"
        disabled={busy || picked.size === 0}
        onClick={() => void update()}
      >
        {busy
          ? by === "rebase" ? "Rebasing…" : "Merging…"
          : `${by === "rebase" ? "Rebase" : "Merge into"} ${picked.size} repo${picked.size === 1 ? "" : "s"}`}
      </button>
    </>
  ) : (
    <>
      {conflicted.length > 0 && <AgentTargetFields task={task} at={at} disabled={busy} />}
      <div className="spacer" />
      <button className="btn" onClick={onClose} disabled={busy}>Close</button>
      {conflicted.length > 0 && (
        <button
          className="btn btn-primary"
          disabled={busy || at.stuck}
          title={at.stuck ? "Install an agent CLI first" : undefined}
          onClick={() => void handOff()}
        >
          {running === 0 ? "Start & resolve" : "Resolve with agent"}
        </button>
      )}
    </>
  );

  return (
    <Modal title="Update from base" onClose={() => !busy && onClose()} footer={footer}>
      {results === null && conflicted.length === 0 && (
        <>
          <div className="section-tabs" style={{ marginBottom: 10 }}>
            {(["merge", "rebase"] as UpdateBy[]).map((b) => (
              <button
                key={b}
                className={by === b ? "active" : ""}
                disabled={busy}
                onClick={() => setBy(b)}
              >
                {b === "merge" ? "Merge" : "Rebase"}
              </button>
            ))}
          </div>
          <p className="muted" style={{ marginTop: 0, lineHeight: 1.55, fontSize: 13 }}>
            Fetches each base first. {HOW[by]} Push afterwards to update the PRs.
          </p>
          {task.checkouts.map((c) => {
            const edited = (c.status?.staged ?? 0) + (c.status?.unstaged ?? 0);
            return (
              <label key={c.id} className="fb-item" style={{ cursor: c.exists ? "pointer" : "default" }}>
                <input
                  type="checkbox"
                  disabled={!c.exists || busy}
                  checked={picked.has(c.id)}
                  onChange={() =>
                    setPicked((p) => {
                      const next = new Set(p);
                      if (next.has(c.id)) next.delete(c.id);
                      else next.add(c.id);
                      return next;
                    })
                  }
                />
                <div className="fb-main">
                  <div className="row">
                    <span className="fb-head mono">{c.project_name}</span>
                    <span className="muted">← origin/{c.base}</span>
                    {!c.exists && <span className="chip del">worktree missing</span>}
                    {edited > 0 && <span className="chip warn">uncommitted changes</span>}
                  </div>
                  {edited > 0 && (
                    <div className="fb-body">Commit or stash them first — this repo will be skipped.</div>
                  )}
                </div>
              </label>
            );
          })}
          {running > 0 && (
            <p className="muted fb-note" style={{ lineHeight: 1.5 }}>
              {running} agent{running === 1 ? " is" : "s are"} running in this task. Files{" "}
              {running === 1 ? "it has" : "they have"} open may change under{" "}
              {running === 1 ? "it" : "them"}.
            </p>
          )}
        </>
      )}

      {busy && results === null && (
        <div className="row muted" style={{ marginTop: 10 }}>
          <Spinner /> Fetching and {by === "rebase" ? "rebasing" : "merging"}…
        </div>
      )}

      {results?.map((r) => (
        <div key={r.checkout_id} className="check" style={{ alignItems: "flex-start" }}>
          <span
            className="dot"
            style={{
              background: aborted.has(r.checkout_id) ? "var(--dimmer)" : OUTCOME_DOT[r.outcome],
              marginTop: 6,
            }}
          />
          <div style={{ flex: 1, minWidth: 0 }}>
            <div className="row">
              <span className="name">{r.repo}</span>
              <span className="muted">
                {aborted.has(r.checkout_id) ? `${ran ?? "update"} abandoned` : r.detail}
              </span>
            </div>
            {r.outcome === "conflicts" && !aborted.has(r.checkout_id) && (
              <div className="fb-files">{r.conflicts.join("\n")}</div>
            )}
          </div>
        </div>
      ))}

      {conflicted.length > 0 && (
        <div style={{ marginTop: results ? 14 : 0 }}>
          <p className="muted" style={{ lineHeight: 1.55, fontSize: 13, marginTop: 0 }}>
            {results
              ? `The ${ran} is left in progress where it stopped, so the conflicts can be resolved.`
              : "An earlier update is still in progress."}{" "}
            An agent can resolve them and finish it, or abandon it here to put the branch back as
            it was.
          </p>
          {conflicted.map((c) => (
            <div key={c.id} className="check">
              <span className="name">{c.project_name}</span>
              <button className="btn btn-sm" disabled={busy} onClick={() => void abort(c.id)}>
                Abandon {ran ?? "update"}
              </button>
            </div>
          ))}
        </div>
      )}
    </Modal>
  );
}
