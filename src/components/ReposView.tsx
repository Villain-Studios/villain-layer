import { useState, type ReactNode } from "react";
import { api } from "../lib/api";
import { groupProjects, useStore } from "../store";
import type { MatchKind, Project } from "../lib/types";
import { AddRepos } from "./AddRepos";
import { Confirm } from "./ui";
import { RepoPicker } from "./RepoPicker";

export function ReposView() {
  const { projects, repoSets, repoRules, tasks } = useStore();
  const refreshRepos = useStore((s) => s.refreshRepos);
  const refreshAll = useStore((s) => s.refreshAll);
  const toast = useStore((s) => s.toast);
  const fail = useStore((s) => s.fail);

  const [adding, setAdding] = useState(false);
  const [shut, setShut] = useState<Record<string, boolean>>({});
  const [editingSet, setEditingSet] = useState<string | null>(null);
  const [ruleKind, setRuleKind] = useState<MatchKind>("component");
  const [ruleValue, setRuleValue] = useState("");
  const [rulePicked, setRulePicked] = useState<string[]>([]);
  const [confirming, setConfirming] = useState<{
    title: string;
    body: ReactNode;
    label: string;
    run: () => void;
  } | null>(null);

  const groups = groupProjects(projects);
  const anyOpen = groups.some((g) => !(shut[g.group] ?? true));
  const names = (ids: string[]) =>
    ids.map((id) => projects.find((p) => p.id === id)?.name ?? "?").join(", ");

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

  async function addRule() {
    try {
      await api.saveRepoRule(ruleKind, ruleValue.trim(), rulePicked);
      await refreshRepos();
      setRuleValue("");
      setRulePicked([]);
      toast("success", "Rule saved");
    } catch (e) {
      fail(e);
    }
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
            <input
              style={{ width: 170 }}
              defaultValue={g.group}
              placeholder="rename group"
              onClick={(e) => e.stopPropagation()}
              onBlur={(e) => {
                if (e.target.value.trim() !== g.group) void regroup(g.group, e.target.value);
              }}
              onKeyDown={(e) => { if (e.key === "Enter") e.currentTarget.blur(); }}
            />
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
                <input
                  list="villain-groups-view"
                  defaultValue={p.group ?? ""}
                  placeholder="ungrouped"
                  onBlur={(e) => {
                    if ((e.target.value.trim() || null) !== p.group) {
                      void setGroup(p.id, e.target.value);
                    }
                  }}
                  onKeyDown={(e) => { if (e.key === "Enter") e.currentTarget.blur(); }}
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

      <datalist id="villain-groups-view">
        {[...new Set(projects.map((p) => p.group).filter(Boolean))].map((g) => (
          <option key={g as string} value={g as string} />
        ))}
      </datalist>

      <div className="card">
        <h3>Saved sets</h3>
        <div className="muted" style={{ marginBottom: 10, lineHeight: 1.55 }}>
          Named repo combinations, applied with one click when starting a task. Create
          them from the picker: select some repos, then “save these as a set”.
        </div>
        {repoSets.length === 0 && <div className="muted">None yet.</div>}
        {repoSets.map((set) => (
          <div key={set.id}>
            <div className="rule-row">
              <span className="val">{set.name}</span>
              <span className="repos">{names(set.project_ids)}</span>
              <button
                className="btn btn-sm"
                onClick={() => setEditingSet(editingSet === set.id ? null : set.id)}
              >
                {editingSet === set.id ? "Done" : "Edit"}
              </button>
              <button
                className="btn btn-sm btn-danger"
                onClick={() => void api.deleteRepoSet(set.id).then(refreshRepos).catch(fail)}
              >
                ✕
              </button>
            </div>
            {editingSet === set.id && (
              <div style={{ margin: "8px 0 14px" }}>
                <RepoPicker
                  projects={projects}
                  picked={set.project_ids}
                  onChange={(ids) => {
                    void api.saveRepoSet(set.name, ids, set.id).then(refreshRepos).catch(fail);
                  }}
                />
              </div>
            )}
          </div>
        ))}
      </div>

      <div className="card">
        <h3>Jira rules</h3>
        <div className="muted" style={{ marginBottom: 10, lineHeight: 1.55 }}>
          “Tickets with this component touch these repos.” Rules are explicit, so they
          outrank what the last similar ticket happened to use.
        </div>
        {repoRules.length === 0 && <div className="muted">None yet.</div>}
        {repoRules.map((rule) => (
          <div key={rule.id} className="rule-row">
            <span className="group-chip">{rule.kind}</span>
            <span className="val">{rule.value}</span>
            <span className="repos">→ {names(rule.project_ids)}</span>
            <button
              className="btn btn-sm btn-danger"
              onClick={() => void api.deleteRepoRule(rule.id).then(refreshRepos).catch(fail)}
            >
              ✕
            </button>
          </div>
        ))}

        <div style={{ marginTop: 12 }}>
          <div className="row" style={{ marginBottom: 8 }}>
            <select
              style={{ width: 140 }}
              value={ruleKind}
              onChange={(e) => setRuleKind(e.target.value as MatchKind)}
            >
              <option value="component">Component</option>
              <option value="label">Label</option>
            </select>
            <input
              placeholder="Payments"
              value={ruleValue}
              onChange={(e) => setRuleValue(e.target.value)}
            />
            <button
              className="btn btn-primary"
              disabled={!ruleValue.trim() || rulePicked.length === 0}
              onClick={() => void addRule()}
            >
              Add rule
            </button>
          </div>
          <RepoPicker projects={projects} picked={rulePicked} onChange={setRulePicked} />
        </div>
      </div>

      {adding && <AddRepos onClose={() => setAdding(false)} />}

      {confirming && (
        <Confirm
          title={confirming.title}
          body={confirming.body}
          confirmLabel={confirming.label}
          onConfirm={confirming.run}
          onCancel={() => setConfirming(null)}
        />
      )}
    </div>
  );
}
