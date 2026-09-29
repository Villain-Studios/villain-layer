import { useEffect, useState } from "react";
import { api } from "../lib/api";
import { useStore } from "../store";
import { updateByFor } from "../lib/derive";
import type { RepoUpdate, TaskView, UpdateBy } from "../lib/types";
import { Combo, Modal, Spinner } from "./ui";
import { AgentTargetFields, useAgentTarget } from "./AgentTarget";

const OUTCOME_DOT: Record<RepoUpdate["outcome"], string> = {
  up_to_date: "var(--dimmer)",
  updated: "var(--green)",
  conflicts: "var(--red)",
  failed: "var(--amber)",
};

/** What each way does, said where the choice is made. */
/** Both at once, for a task whose repos go different ways. */
const HOW_BOTH =
  "Merge keeps the branch's history, so its pull request needs no force push. Rebase replays it on top of the base for a straight line, and the next push replaces the remote branch — only if nobody else has pushed to it since.";

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
  const refreshRepos = useStore((s) => s.refreshRepos);
  const projects = useStore((s) => s.projects);
  const health = useStore((s) => s.repoHealth);
  const toast = useStore((s) => s.toast);
  const fail = useStore((s) => s.fail);
  const githubConnected = useStore((s) => s.settings?.github_connected ?? false);
  const setTaskPrs = useStore((s) => s.setTaskPrs);
  const at = useAgentTarget(task);

  /**
   * What each repo's base field offers: its origin branches as last fetched.
   *
   * The base is changed here, where it is read. Only the Open pull request
   * dialog had the field, and that is shut once every repo has a PR, so a
   * base merged and deleted upstream could not be moved off at all (UPD-8).
   */
  const [branches, setBranches] = useState<Record<string, string[]>>({});
  const existing = task.checkouts.filter((c) => c.exists).map((c) => c.id).join(",");
  useEffect(() => {
    let stop = false;
    void (async () => {
      const ids = existing.split(",").filter(Boolean);
      const lists = await Promise.all(
        ids.map((id) => api.checkoutBranches(id).catch(() => [] as string[])),
      );
      if (!stop) setBranches(Object.fromEntries(ids.map((id, i) => [id, lists[i]])));
    })();
    return () => { stop = true; };
  }, [existing]);

  const [picked, setPicked] = useState<Set<string>>(
    () => new Set(task.checkouts.filter((c) => c.exists).map((c) => c.id)),
  );
  const [results, setResults] = useState<RepoUpdate[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [aborted, setAborted] = useState<Set<string>>(new Set());
  // Each repo its own team's way (UPD-7): one choice for the whole task
  // was wrong as soon as a task spanned a team that merges and one that
  // rebases.
  const way = (projectId: string) =>
    updateByFor(projects.find((p) => p.id === projectId), health[projectId]);
  const [by, setBy] = useState<Record<string, UpdateBy>>(
    () => Object.fromEntries(task.checkouts.map((c) => [c.id, way(c.project_id).by])),
  );
  /** What each repo's result was produced by, for wording it. */
  const [ran, setRan] = useState<Record<string, UpdateBy> | null>(null);
  const modes = new Set([...picked].map((id) => by[id] ?? "merge"));
  const only = modes.size === 1 ? [...modes][0] : null;

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
      const chosen = Object.fromEntries([...picked].map((id) => [id, by[id] ?? "merge"])) as Record<string, UpdateBy>;
      const out = await api.updateFromBase(
        task.id,
        Object.entries(chosen).map(([checkout_id, b]) => ({ checkout_id, by: b })),
      );
      setResults(out);
      setRan(chosen);
      setAborted(new Set());
      const updated = out.filter((r) => r.outcome === "updated");
      const clashes = out.filter((r) => r.outcome === "conflicts").length;
      if (updated.length > 0 && clashes === 0) {
        const rebased = updated.some((r) => chosen[r.checkout_id] === "rebase");
        toast(
          "success",
          `Brought ${updated.length} repo${updated.length === 1 ? "" : "s"} up to date` +
            (rebased ? " — Push all replaces the remote branches that were rebased" : ""),
        );
      }
      await Promise.all([refreshTasks().catch(() => {}), refreshRepos().catch(() => {})]);
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  async function setBase(checkoutId: string, base: string) {
    const was = task.checkouts.find((c) => c.id === checkoutId)?.base;
    if (!base.trim() || base.trim() === was) return;
    try {
      await api.setCheckoutBase(checkoutId, base);
      // The Pull requests tab reads the base from its own rows, and offered
      // to move the PR back onto the old one until the next sweep.
      await Promise.all([
        refreshTasks().catch(() => {}),
        githubConnected
          ? api.githubTaskPrs(task.id).then((rows) => setTaskPrs(task.id, rows)).catch(() => {})
          : null,
      ]);
    } catch (e) {
      fail(e);
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

  /** Only once the list is in: an empty one is a repo not yet listed. */
  const gone = (id: string, base: string) =>
    (branches[id]?.length ?? 0) > 0 && !branches[id].includes(base);

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
          ? only === "rebase" ? "Rebasing…" : only === "merge" ? "Merging…" : "Updating…"
          : `${only === "rebase" ? "Rebase" : only === "merge" ? "Merge into" : "Update"} ${picked.size} repo${picked.size === 1 ? "" : "s"}`}
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
          <p className="muted" style={{ marginTop: 0, lineHeight: 1.55, fontSize: 13 }}>
            Fetches each base first.{" "}
            {only ? HOW[only] : HOW_BOTH} Push afterwards to
            update the PRs. Each repo starts the way its team updates branches; hover to see why.
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
                    {!c.exists && <span className="chip del">worktree missing</span>}
                    {gone(c.id, c.base) && (
                      <span className="chip del" title="Not among origin's branches at the last fetch — merged and deleted, perhaps. Pick the branch it went into.">
                        not on origin
                      </span>
                    )}
                    {edited > 0 && <span className="chip warn">uncommitted changes</span>}
                    <div className="spacer" />
                    {/* Inside the label, so a click here would tick the repo too. */}
                    <span
                      className="by-toggle"
                      title={way(c.project_id).why}
                      onClick={(e) => e.preventDefault()}
                    >
                      {(["merge", "rebase"] as UpdateBy[]).map((b) => (
                        <button
                          key={b}
                          type="button"
                          className={(by[c.id] ?? "merge") === b ? "active" : ""}
                          disabled={!c.exists || busy}
                          onClick={() => setBy((m) => ({ ...m, [c.id]: b }))}
                        >
                          {b === "merge" ? "Merge" : "Rebase"}
                        </button>
                      ))}
                    </span>
                  </div>
                  <div className="row" style={{ marginTop: 4 }}>
                    <span className="muted" style={{ whiteSpace: "nowrap" }}>← origin/</span>
                    {/* Inside the label too: picking from the list would tick the repo. */}
                    <span onClick={(e) => e.preventDefault()}>
                      <Combo
                        value={c.base}
                        options={branches[c.id] ?? []}
                        width={260}
                        title="The branch this repository is updated from, and its pull request opened against"
                        empty="No branch matches"
                        onChange={(v) => void setBase(c.id, v)}
                      />
                    </span>
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
          <Spinner /> Fetching and {only === "rebase" ? "rebasing" : only === "merge" ? "merging" : "updating"}…
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
                {aborted.has(r.checkout_id) ? `${ran?.[r.checkout_id] ?? "update"} abandoned` : r.detail}
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
              ? "What stopped is left in progress where it stopped, so the conflicts can be resolved."
              : "An earlier update is still in progress."}{" "}
            An agent can resolve them and finish it, or abandon it here to put the branch back as
            it was.
          </p>
          {conflicted.map((c) => (
            <div key={c.id} className="check">
              <span className="name">{c.project_name}</span>
              <button className="btn btn-sm" disabled={busy} onClick={() => void abort(c.id)}>
                Abandon {ran?.[c.id] ?? "update"}
              </button>
            </div>
          ))}
        </div>
      )}
    </Modal>
  );
}
