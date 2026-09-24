import { useMemo, useState } from "react";
import { groupProjects } from "../lib/derive";
import { ChevronIcon } from "./icons";
import type { Project } from "../lib/types";

/**
 * Multi-select over the registered repositories. Past ~10 repos a flat
 * checkbox list stops working, so this adds search, collapsible groups with
 * select-all, and saved sets for combinations that recur.
 */
export function RepoPicker({
  projects, picked, onChange, reason,
}: {
  projects: Project[];
  picked: string[];
  onChange: (ids: string[]) => void;
  /** Why these were preselected, surfaced so the guess is auditable. */
  reason?: string | null;
}) {

  const [query, setQuery] = useState("");
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({});

  const groups = useMemo(() => {
    const q = query.trim().toLowerCase();
    const matching = q
      ? projects.filter(
          (p) => p.name.toLowerCase().includes(q) || (p.group ?? "").toLowerCase().includes(q),
        )
      : projects;
    return groupProjects(matching);
  }, [projects, query]);

  function toggle(id: string) {
    onChange(picked.includes(id) ? picked.filter((p) => p !== id) : [...picked, id]);
  }

  function setGroup(ids: string[], on: boolean) {
    onChange(on ? [...new Set([...picked, ...ids])] : picked.filter((p) => !ids.includes(p)));
  }

  if (projects.length === 0) {
    return <div className="muted">No repositories registered yet.</div>;
  }

  const total = projects.length;

  return (
    <div className="picker">
      {reason && (
        <div className="picker-reason">Preselected from {reason} — change freely.</div>
      )}

      {total > 6 && (
        <div className="picker-top">
          <input
            type="text"
            placeholder={`Filter ${total} repositories…`}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
          <span className="muted" style={{ whiteSpace: "nowrap", fontSize: 11 }}>
            {picked.length} selected
          </span>
          {picked.length > 0 && (
            <button className="btn-sm" title="Clear selection" onClick={() => onChange([])}>
              ✕
            </button>
          )}
        </div>
      )}

      <div className="picker-list">
        {groups.length === 0 && (
          <div className="muted" style={{ padding: 12 }}>Nothing matches “{query}”.</div>
        )}
        {groups.map((g) => {
          const ids = g.projects.map((p) => p.id);
          const all = ids.every((id) => picked.includes(id));
          const isCollapsed = collapsed[g.group] ?? false;
          return (
            <div key={g.group || "_"}>
              <div
                className="group-head"
                onClick={() => setCollapsed((c) => ({ ...c, [g.group]: !isCollapsed }))}
              >
                <span className={`chev${isCollapsed ? "" : " open"}`}><ChevronIcon /></span>
                {g.label}
                <span className="count">{g.projects.length}</span>
                <div className="spacer" />
                <div className="acts" onClick={(e) => e.stopPropagation()}>
                  <button onClick={() => setGroup(ids, !all)}>{all ? "none" : "all"}</button>
                </div>
              </div>
              {!isCollapsed && g.projects.map((p) => (
                <label key={p.id} className="repo-pick">
                  <input
                    type="checkbox"
                    checked={picked.includes(p.id)}
                    onChange={() => toggle(p.id)}
                  />
                  <span style={{ flex: 1 }}>{p.name}</span>
                  <span className="path">{p.path}</span>
                </label>
              ))}
            </div>
          );
        })}
      </div>
    </div>
  );
}
