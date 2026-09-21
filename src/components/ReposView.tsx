import { useState, type ReactNode } from "react";
import { api } from "../lib/api";
import { groupProjects, useStore } from "../store";
import type { Project } from "../lib/types";
import { AddRepos } from "./AddRepos";
import { Combo, Confirm } from "./ui";

export function ReposView() {
  const projects = useStore((s) => s.projects);
  const tasks = useStore((s) => s.tasks);
  const refreshRepos = useStore((s) => s.refreshRepos);
  const refreshAll = useStore((s) => s.refreshAll);
  const fail = useStore((s) => s.fail);

  const [adding, setAdding] = useState(false);
  const [shut, setShut] = useState<Record<string, boolean>>({});
  const [confirming, setConfirming] = useState<{
    title: string;
    body: ReactNode;
    label: string;
    busyLabel?: string;
    run: () => void | Promise<unknown>;
  } | null>(null);

  const groups = groupProjects(projects);
  const groupNames = [...new Set(projects.map((p) => p.group).filter(Boolean))] as string[];
  const anyOpen = groups.some((g) => !(shut[g.group] ?? true));

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
          await refreshAll();
        } catch (e) {
          fail(e);
        }
      },
    });
  }

  return (
    <div className="wide">
      <div className="wide-head">
        <h2>Repositories</h2>
        <span className="sub">
          {projects.length} in {groups.length} group{groups.length === 1 ? "" : "s"}
        </span>
        <div className="spacer" />
        {groups.length > 1 && (
          <button
            className="btn btn-sm"
            onClick={() =>
              // Groups start collapsed, so an empty record cannot mean "open".
              // Every group is written either way, or the ones never touched
              // would fall back to the default instead of following.
              setShut(Object.fromEntries(groups.map((g) => [g.group, anyOpen])))
            }
          >
            {anyOpen ? "Collapse all" : "Expand all"}
          </button>
        )}
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
        // Collapsed by default: this tab is for finding one repository among
        // many, and a group is a heading long before it is a list.
        const closed = shut[g.group] ?? true;
        return (
        <div key={g.group || "_"} className="card">
          <div
            className="row"
            style={{ marginBottom: closed ? 0 : 8, cursor: "pointer" }}
            onClick={() => setShut((c) => ({ ...c, [g.group]: !closed }))}
          >
            <span className={`chev${closed ? "" : " open"}`}>▶</span>
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
            const inUse = tasks.filter((t) =>
              t.checkouts.some((c) => c.project_id === p.id),
            ).length;
            return (
              <div key={p.id} className="repo-manage">
                <span className="rname">{p.name}</span>
                <span className="rpath" title={p.path}>{p.path}</span>
                <span className="chip">{p.default_branch}</span>
                {inUse > 0 && <span className="chip add">{inUse} task{inUse === 1 ? "" : "s"}</span>}
                <Combo
                  value={p.group ?? ""}
                  options={groupNames}
                  placeholder="ungrouped"
                  width={170}
                  onChange={(v) => {
                    if ((v.trim() || null) !== p.group) void setGroup(p.id, v);
                  }}
                />
                <button className="btn btn-sm btn-danger" onClick={() => askRemove(p)}>
                  Remove
                </button>
              </div>
            );
          })}
        </div>
        );
      })}

      {adding && <AddRepos onClose={() => setAdding(false)} />}

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
