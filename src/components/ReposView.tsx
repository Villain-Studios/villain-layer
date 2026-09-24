import { useEffect, useState, type ReactNode } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { api } from "../lib/api";
import { ago } from "../lib/time";
import { useNow, useStore } from "../store";
import { groupProjects, repoTrouble, updateByFor } from "../lib/derive";
import type { Project, RepoHealth, Synced, UpdateBy } from "../lib/types";
import { AddRepos } from "./AddRepos";
import { CleanUp } from "./CleanUp";
import { ChevronIcon } from "./icons";
import { Combo, Confirm, Spinner } from "./ui";

export function ReposView() {
  const projects = useStore((s) => s.projects);
  const tasks = useStore((s) => s.tasks);
  const health = useStore((s) => s.repoHealth);
  const refreshRepos = useStore((s) => s.refreshRepos);
  const refreshRepoHealth = useStore((s) => s.refreshRepoHealth);
  const refreshAll = useStore((s) => s.refreshAll);
  const toast = useStore((s) => s.toast);
  const fail = useStore((s) => s.fail);
  const now = useNow(60_000);

  const [adding, setAdding] = useState(false);
  const [cleaning, setCleaning] = useState(false);
  const [shut, setShut] = useState<Record<string, boolean>>({});
  const [syncing, setSyncing] = useState<Set<string>>(new Set());
  const [synced, setSynced] = useState<Record<string, Synced>>({});
  const [confirming, setConfirming] = useState<{
    title: string;
    body: ReactNode;
    label: string;
    busyLabel?: string;
    run: () => void | Promise<unknown>;
  } | null>(null);

  // Read again whenever the view opens: what it says is only as fresh as
  // the last look (REPO-5).
  useEffect(() => { void refreshRepoHealth().catch(fail); }, [refreshRepoHealth, fail]);

  const groups = groupProjects(projects);
  const groupNames = [...new Set(projects.map((p) => p.group).filter(Boolean))] as string[];
  const trouble = (p: Project) => repoTrouble(p, health[p.id], tasks);
  const troubled = projects.filter((p) => trouble(p)).length;
  // Collapsed by default: this tab is for finding one repository among
  // many, and a group is a heading long before it is a list. A group with
  // a repo in trouble opens, or the trouble is a count nobody can find.
  const isClosed = (g: (typeof groups)[number]) => shut[g.group] ?? !g.projects.some((p) => trouble(p));
  const anyOpen = groups.some((g) => !isClosed(g));
  const lastSync = Math.max(0, ...projects.map((p) => health[p.id]?.synced_at ?? 0));

  async function setGroup(projectId: string, group: string) {
    try {
      await api.setProjectGroup([projectId], group.trim() || null);
      await refreshRepos();
    } catch (e) {
      fail(e);
    }
  }

  async function regroup(from: string, to: string) {
    const ids = projects.filter((p) => (p.group ?? "") === from).map((p) => p.id);
    try {
      await api.setProjectGroup(ids, to.trim() || null);
      await refreshRepos();
    } catch (e) {
      fail(e);
    }
  }

  async function setUpdateBy(p: Project, by: UpdateBy | null) {
    try {
      await api.setProjectUpdateBy(p.id, by);
      await refreshRepos();
    } catch (e) {
      fail(e);
    }
  }

  async function sync(ids: string[]) {
    setSyncing((s) => new Set([...s, ...ids]));
    try {
      const rows = await api.syncRepos(ids);
      setSynced((s) => ({ ...s, ...Object.fromEntries(rows.map((r) => [r.project_id, r])) }));
      const failed = rows.filter((r) => !r.ok);
      if (ids.length > 1) {
        toast(
          failed.length ? "error" : "success",
          failed.length
            ? `Synced ${rows.length - failed.length} of ${rows.length}. Each repo says what happened.`
            : `Synced ${rows.length} repositories.`,
        );
      } else if (failed.length) {
        toast("error", failed[0].detail);
      }
      await Promise.all([refreshRepos(), refreshRepoHealth()]);
    } catch (e) {
      fail(e);
    } finally {
      setSyncing((s) => new Set([...s].filter((id) => !ids.includes(id))));
    }
  }

  async function locate(p: Project, path?: string) {
    const chosen = path ?? (await openDialog({ directory: true, title: `Where is ${p.name} now?` }));
    if (typeof chosen !== "string") return;
    try {
      const moved = await api.locateProject(p.id, chosen);
      // Its group was open only for the trouble; closing under the click
      // that fixed it hides what just happened.
      setShut((c) => ({ ...c, [p.group ?? ""]: false }));
      toast("success", `${p.name} is at ${moved.path} now. Its tasks keep it.`);
      await Promise.all([refreshRepos(), refreshRepoHealth()]);
    } catch (e) {
      fail(e);
    }
  }

  function askRemove(p: Project) {
    const using = tasks.filter((t) => t.checkouts.some((c) => c.project_id === p.id));
    setConfirming({
      title: "Remove repository",
      label: "Remove",
      body: (
        <>
          Remove <b>{p.name}</b> from Villain Layer? The clone at <code>{p.path}</code>{" "}
          and any worktrees stay on disk untouched — only this app forgets about it.
          {using.length > 0 && (
            <div className="confirm-detail">
              {using.length} task{using.length === 1 ? "" : "s"} currently use it. They
              lose that repository, and any task left with none is removed.
              {health[p.id]?.clone !== "ok" && " If it has only moved, Locate it instead."}
            </div>
          )}
        </>
      ),
      // `refreshAll` is the slowest read in the app — a git status of every
      // worktree — and dropping a repo used by several tasks takes those with
      // it. Awaited, so the dialog is what waits rather than the sidebar
      // silently rearranging itself a few seconds later.
      busyLabel: "Removing…",
      run: async () => {
        try {
          await api.removeProject(p.id);
          await Promise.all([refreshAll(), refreshRepoHealth()]);
        } catch (e) {
          fail(e);
        }
      },
    });
  }

  const syncingAll = syncing.size > 1;

  return (
    <div className="wide">
      <div className="wide-head">
        <h2>Repositories</h2>
        <span className="sub">
          {projects.length} in {groups.length} group{groups.length === 1 ? "" : "s"}
          {troubled > 0 && <> · <span className="trouble-text">{troubled} to fix</span></>}
          {lastSync > 0 && <> · last synced {ago(lastSync * 1000, now)}</>}
        </span>
        <div className="spacer" />
        {groups.length > 1 && (
          <button
            className="btn btn-sm"
            onClick={() =>
              // Every group is written either way, or the ones never touched
              // would fall back to the default instead of following.
              setShut(Object.fromEntries(groups.map((g) => [g.group, anyOpen])))
            }
          >
            {anyOpen ? "Collapse all" : "Expand all"}
          </button>
        )}
        <button
          className="btn"
          disabled={projects.length === 0}
          title="Remove what tasks left behind. Shows everything first."
          onClick={() => setCleaning(true)}
        >
          Clean up…
        </button>
        <button
          className="btn"
          disabled={projects.length === 0 || syncing.size > 0}
          title="Fetch every repo into the app's copy, and fast-forward each clone's default branch where that is safe"
          onClick={() => void sync(projects.map((p) => p.id))}
        >
          {syncingAll ? <span className="btn-busy"><Spinner />Syncing…</span> : "Sync all"}
        </button>
        <button className="btn btn-primary" onClick={() => setAdding(true)}>
          Add repositories
        </button>
      </div>

      {projects.length === 0 && (
        <div className="card">
          <div className="muted" style={{ lineHeight: 1.6 }}>
            None registered yet. <b>Add repositories</b> can scan a parent folder and
            pick up a whole stack at once.
          </div>
        </div>
      )}

      {groups.map((g) => {
        const closed = isClosed(g);
        return (
        <div key={g.group || "_"} className="card">
          <div
            className="row"
            style={{ marginBottom: closed ? 0 : 8, cursor: "pointer" }}
            onClick={() => setShut((c) => ({ ...c, [g.group]: !closed }))}
          >
            <span className={`chev${closed ? "" : " open"}`}><ChevronIcon /></span>
            <h3 style={{ margin: 0 }}>{g.label}</h3>
            <span className="muted">{g.projects.length}</span>
            <div className="spacer" />
            {/* Renaming must not toggle the section underneath it. */}
            <div onClick={(e) => e.stopPropagation()}>
              <Combo
                value={g.group}
                options={groupNames}
                placeholder="rename group"
                width={170}
                onChange={(v) => { if (v.trim() !== g.group) void regroup(g.group, v); }}
              />
            </div>
          </div>

          {!closed && g.projects.map((p) => {
            const h = health[p.id];
            const problem = trouble(p);
            const unlinked = problem !== null && h?.clone === "ok";
            const inUse = tasks.filter((t) =>
              t.checkouts.some((c) => c.project_id === p.id),
            ).length;
            const row = synced[p.id];
            const busy = syncing.has(p.id);
            return (
              <div key={p.id} className={`repo-manage${problem ? " trouble" : ""}`}>
                <div className="repo-line">
                  <span className="rname">{p.name}</span>
                  <span className="chip" title="Default branch">{p.default_branch}</span>
                  <Standing h={h} branch={p.default_branch} />
                  {inUse > 0 && <span className="chip add">{inUse} task{inUse === 1 ? "" : "s"}</span>}
                  <CopyState h={h} now={now} />
                  <div className="spacer" />
                  <UpdateByPicker p={p} h={h} onChange={(by) => void setUpdateBy(p, by)} />
                  <Combo
                    value={p.group ?? ""}
                    options={groupNames}
                    placeholder="ungrouped"
                    width={150}
                    onChange={(v) => {
                      if ((v.trim() || null) !== p.group) void setGroup(p.id, v);
                    }}
                  />
                  <button className="btn btn-sm" disabled={busy} onClick={() => void sync([p.id])}>
                    {busy ? <span className="btn-busy"><Spinner />Syncing…</span> : "Sync"}
                  </button>
                  <button className="btn btn-sm btn-danger" onClick={() => askRemove(p)}>
                    Remove
                  </button>
                </div>
                <div className="repo-sub">
                  {/* Isolated, or the right-to-left run that keeps the end in
                      view moves the leading slash to the end. */}
                  <span className="rpath" title={p.path}><bdi>{p.path}</bdi></span>
                  {h?.origin && <span className="rorigin" title={`Fetches from ${h.origin}`}>{h.origin}</span>}
                </div>
                {problem && (
                  <div className="repo-problem">
                    <span>
                      {problem}
                      {unlinked && " They are linked back when the app next starts, if the app's copy has their last commit."}
                    </span>
                    {h?.found && (
                      <button className="btn btn-sm btn-primary" onClick={() => void locate(p, h.found ?? undefined)}>
                        Use {h.found}
                      </button>
                    )}
                    {!unlinked && (
                      <button className="btn btn-sm" onClick={() => void locate(p)}>Locate…</button>
                    )}
                  </div>
                )}
                {row && <div className={`repo-synced${row.ok ? "" : " failed"}`}>{row.detail}</div>}
              </div>
            );
          })}
        </div>
        );
      })}

      {adding && <AddRepos onClose={() => { setAdding(false); void refreshRepoHealth().catch(fail); }} />}

      {cleaning && (
        <CleanUp
          onClose={() => setCleaning(false)}
          onDone={() => void Promise.all([refreshAll(), refreshRepoHealth()]).catch(fail)}
        />
      )}

      {confirming && (
        <Confirm
          title={confirming.title}
          body={confirming.body}
          confirmLabel={confirming.label}
          busyLabel={confirming.busyLabel}
          onConfirm={confirming.run}
          onCancel={() => setConfirming(null)}
        />
      )}
    </div>
  );
}

/** The clone's default branch against origin's, when it is not level. */
function Standing({ h, branch }: { h: RepoHealth | undefined; branch: string }) {
  if (!h || h.behind === null || h.ahead === null) return null;
  if (h.ahead > 0) {
    return (
      <span
        className="chip warn"
        title={`Your clone's ${branch} has ${h.ahead} commit${h.ahead === 1 ? "" : "s"} origin does not${h.behind ? `, and is ${h.behind} behind` : ""}. Sync leaves it alone.`}
      >
        ↑{h.ahead}{h.behind > 0 && ` ↓${h.behind}`}
      </span>
    );
  }
  if (h.behind > 0) {
    return (
      <span className="chip" title={`Your clone's ${branch} is ${h.behind} commit${h.behind === 1 ? "" : "s"} behind origin. Sync fast-forwards it.`}>
        ↓{h.behind}
      </span>
    );
  }
  return null;
}

/**
 * How Update from base updates branches here (UPD-7). Unset, it follows
 * the guess from history, and the first option says what that is.
 */
function UpdateByPicker({ p, h, onChange }: {
  p: Project;
  h: RepoHealth | undefined;
  onChange: (by: UpdateBy | null) => void;
}) {
  const guess = updateByFor(undefined, h);
  return (
    <select
      className="update-by"
      value={p.update_by ?? ""}
      title={`How Update from base brings the base in. ${updateByFor(p, h).why}`}
      onChange={(e) => onChange((e.target.value || null) as UpdateBy | null)}
    >
      <option value="">{h?.update_guess ? `${guess.by}s (guessed)` : "merges (default)"}</option>
      <option value="merge">merges</option>
      <option value="rebase">rebases</option>
    </select>
  );
}

/** Whether the app's copy exists, and when a Sync last reached origin. */
function CopyState({ h, now }: { h: RepoHealth | undefined; now: number }) {
  if (!h) return null;
  if (!h.store) {
    return <span className="rstore warn" title="Sync makes it, if the clone is there">no copy yet</span>;
  }
  return (
    <span className="rstore" title={`The app's copy: ${h.store}`}>
      {h.synced_at ? `synced ${ago(h.synced_at * 1000, now)}` : "not synced yet"}
    </span>
  );
}
