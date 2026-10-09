import { useEffect, useState } from "react";
import { api, errMessage } from "../lib/api";
import type { FlowStatus, JiraTransition, ProjectStatus, Settings, StartTo, TicketFlow } from "../lib/types";
import { useStore } from "../store";
import { Field } from "./ui";

/** The flow chosen for a ticket's project, if any. */
export function flowOf(settings: Settings | null, key: string | null): TicketFlow | undefined {
  return key ? settings?.jira?.flow?.[key.split("-")[0]] : undefined;
}

/**
 * The transition that closes a ticket whose work has merged: into the
 * status chosen for its project, else the only done one there is. None
 * when that is a guess: Done and Cancelled are both "done", and the first
 * in the list once closed a merged ticket as Won't Do.
 */
export function mergedTransition(all: JiraTransition[], flow: TicketFlow | undefined): JiraTransition | undefined {
  const done = all.filter((t) => t.to_category === "done");
  return all.find((t) => flow?.merged && t.to_id === flow.merged.id) ?? (done.length === 1 ? done[0] : undefined);
}

/**
 * Where tickets go as the work moves (TKT-1, TKT-8), per Jira project.
 * Chosen here once: Review and In Progress share Jira's "in progress"
 * category, and the app never goes by a status's name. Where a ticket goes
 * when work starts was a switch in General, out of sight of the rest.
 */
export function TicketFlowSettings() {
  const settings = useStore((s) => s.settings);
  const tasks = useStore((s) => s.tasks);
  const refreshSettings = useStore((s) => s.refreshSettings);
  const fail = useStore((s) => s.fail);
  // The project the ticket list is scoped to, and every one there is work in.
  const projects = [...new Set([
    ...(settings?.jira?.project_key ? [settings.jira.project_key] : []),
    ...tasks.map((t) => t.issue_key?.split("-")[0]).filter((k): k is string => !!k),
  ])].sort();
  const [statuses, setStatuses] = useState<Record<string, ProjectStatus[] | string>>({});
  const wanted = projects.join(",");

  useEffect(() => {
    for (const p of wanted.split(",").filter(Boolean)) {
      api.jiraProjectStatuses(p).then(
        (list) => setStatuses((s) => ({ ...s, [p]: list })),
        (e) => setStatuses((s) => ({ ...s, [p]: errMessage(e) })),
      );
    }
  }, [wanted]);

  async function chooseStart(project: string, value: string) {
    const list = statuses[project];
    const picked = Array.isArray(list) ? list.find((s) => s.id === value) : undefined;
    const to: StartTo = picked
      ? { to: "status", id: picked.id, name: picked.name }
      : value === "leave" ? { to: "leave" } : { to: "first_in_progress" };
    try {
      await api.setTicketStart(project, to);
      await refreshSettings();
    } catch (e) {
      fail(e);
    }
  }

  async function choose(project: string, stage: "review" | "merged", id: string) {
    const list = statuses[project];
    const picked = Array.isArray(list) ? list.find((s) => s.id === id) : undefined;
    const status: FlowStatus | null = picked ? { id: picked.id, name: picked.name } : null;
    try {
      await api.setTicketFlow(project, stage, status);
      await refreshSettings();
    } catch (e) {
      fail(e);
    }
  }

  if (projects.length === 0) return null;
  return (
    <div style={{ marginTop: 22 }}>
      <Field
        label="Tickets follow the work"
        hint="Where a ticket goes when work on it starts, when a pull request of its task is ready for review (not a draft), and when every one has merged. Each happens once per task: a ticket you move by hand afterwards stays where you put it."
      >
        {projects.map((p) => {
          const list = statuses[p];
          const flow = settings?.jira?.flow?.[p];
          const pick = (stage: "review" | "merged", chosen: FlowStatus | null | undefined) => (
            <select
              value={chosen?.id ?? ""}
              style={{ width: "auto" }}
              onChange={(e) => void choose(p, stage, e.target.value)}
            >
              <option value="">leave the ticket</option>
              {Array.isArray(list) && list.filter((s) => s.category !== "new").map((s) => (
                <option key={s.id} value={s.id}>{s.name}</option>
              ))}
            </select>
          );
          // Until chosen here, as the switch that came before said (TKT-1).
          const started = flow?.started ?? (settings?.ui.sync_jira_status === false ? { to: "leave" } : { to: "first_in_progress" });
          const start = (
            <select
              value={started.to === "status" ? started.id : started.to === "leave" ? "leave" : ""}
              style={{ width: "auto" }}
              onChange={(e) => void chooseStart(p, e.target.value)}
            >
              <option value="">first in-progress status</option>
              {Array.isArray(list) && list.filter((s) => s.category === "indeterminate").map((s) => (
                <option key={s.id} value={s.id}>{s.name}</option>
              ))}
              <option value="leave">leave the ticket</option>
            </select>
          );
          return (
            <div key={p} className="row" style={{ gap: 10, flexWrap: "wrap", marginBottom: 8, fontSize: 13 }}>
              <b className="mono" style={{ minWidth: 60 }}>{p}</b>
              {typeof list === "string" ? (
                <span style={{ color: "var(--red)" }}>{list}</span>
              ) : !list ? (
                <span className="muted">Reading its statuses…</span>
              ) : (
                <>
                  <span className="muted">work starts →</span>
                  {start}
                  <span className="muted">PR ready for review →</span>
                  {pick("review", flow?.review)}
                  <span className="muted">every PR merged →</span>
                  {pick("merged", flow?.merged)}
                </>
              )}
            </div>
          );
        })}
      </Field>
    </div>
  );
}
