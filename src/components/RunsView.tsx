import { useEffect, useState } from "react";

import { api } from "../lib/api";
import { ago } from "../lib/time";
import { useNow, useStore } from "../store";
import { SidebarToggle, Spinner, Confirm } from "./ui";
import type { AgentRun, RunStats } from "../lib/types";

export function RunsView() {
  const agents = useStore((s) => s.agents);
  const projects = useStore((s) => s.projects);
  const fail = useStore((s) => s.fail);
  const now = useNow(30_000);

  const [runs, setRuns] = useState<AgentRun[]>([]);
  const [stats, setStats] = useState<RunStats | null>(null);
  const [loading, setLoading] = useState(true);
  const [filterAgent, setFilterAgent] = useState<string | null>(null);
  const [filterProject, setFilterProject] = useState<string | null>(null);
  const [expandedRun, setExpandedRun] = useState<string | null>(null);
  const [confirmClear, setConfirmClear] = useState(false);

  useEffect(() => {
    loadRuns();
  }, [filterAgent, filterProject]);

  async function loadRuns() {
    setLoading(true);
    try {
      const [runsData, statsData] = await Promise.all([
        api.listRuns(filterAgent, filterProject),
        api.runStats(filterAgent, filterProject),
      ]);
      setRuns(runsData);
      setStats(statsData);
    } catch (e) {
      fail(e);
    } finally {
      setLoading(false);
    }
  }

  async function clearData() {
    try {
      await api.clearRuns();
      setConfirmClear(false);
      await loadRuns();
    } catch (e) {
      fail(e);
    }
  }

  const agentMap = new Map(agents.map((a) => [a.id, a.name]));

  function resultIcon(result: string) {
    switch (result) {
      case "success":
        return <span style={{ color: "var(--green)" }}>✓</span>;
      case "failure":
        return <span style={{ color: "var(--red)" }}>✗</span>;
      case "stopped":
        return <span style={{ color: "var(--grey)" }}>■</span>;
      case "error":
        return <span style={{ color: "var(--amber)" }}>⚠</span>;
      default:
        return null;
    }
  }

  function formatDuration(secs: number) {
    if (secs < 60) return `${secs.toFixed(1)}s`;
    const mins = Math.floor(secs / 60);
    const s = Math.floor(secs % 60);
    if (mins < 60) return `${mins}m ${s}s`;
    const hrs = Math.floor(mins / 60);
    const m = mins % 60;
    return `${hrs}h ${m}m`;
  }

  function formatTokens(tokens: { input: number; output: number; total: number } | null) {
    if (!tokens) return "—";
    return `${(tokens.total / 1000).toFixed(1)}k`;
  }

  return (
    <div className="wide">
      <div className="wide-head">
        <SidebarToggle />
        <h2>Run History</h2>
        <span className="sub">
          {stats && (
            <>
              {stats.total} run{stats.total === 1 ? "" : "s"} · {stats.success} passed ·{" "}
              {stats.failure + stats.error} failed · avg{" "}
              {formatDuration(stats.avg_duration)}
            </>
          )}
        </span>
        <div className="spacer" />
        <button className="btn btn-sm" onClick={() => void loadRuns()}>
          Refresh
        </button>
        {runs.length > 0 && (
          <button
            className="btn btn-sm btn-danger"
            onClick={() => setConfirmClear(true)}
          >
            Clear All
          </button>
        )}
      </div>

      <div style={{ padding: 16, display: "flex", gap: 12, borderBottom: "1px solid var(--divider)" }}>
        <label style={{ display: "flex", gap: 6, alignItems: "center" }}>
          <span style={{ fontSize: 13, color: "var(--grey)" }}>Agent:</span>
          <select
            value={filterAgent ?? ""}
            onChange={(e) => setFilterAgent(e.target.value || null)}
            style={{
              padding: "4px 8px",
              fontSize: 13,
              background: "var(--panel-bg)",
              color: "var(--text)",
              border: "1px solid var(--divider)",
              borderRadius: 4,
            }}
          >
            <option value="">All</option>
            {agents.map((a) => (
              <option key={a.id} value={a.id}>
                {a.name}
              </option>
            ))}
          </select>
        </label>

        <label style={{ display: "flex", gap: 6, alignItems: "center" }}>
          <span style={{ fontSize: 13, color: "var(--grey)" }}>Repository:</span>
          <select
            value={filterProject ?? ""}
            onChange={(e) => setFilterProject(e.target.value || null)}
            style={{
              padding: "4px 8px",
              fontSize: 13,
              background: "var(--panel-bg)",
              color: "var(--text)",
              border: "1px solid var(--divider)",
              borderRadius: 4,
            }}
          >
            <option value="">All</option>
            {projects.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </select>
        </label>
      </div>

      {stats && stats.total > 0 && (
        <div
          style={{
            padding: 16,
            display: "grid",
            gridTemplateColumns: "repeat(auto-fit, minmax(140px, 1fr))",
            gap: 12,
            borderBottom: "1px solid var(--divider)",
          }}
        >
          <div className="stat">
            <div className="label">Success Rate</div>
            <div className="value">
              {((stats.success / stats.total) * 100).toFixed(1)}%
            </div>
          </div>
          <div className="stat">
            <div className="label">Avg Duration</div>
            <div className="value">{formatDuration(stats.avg_duration)}</div>
          </div>
          <div className="stat">
            <div className="label">Errors</div>
            <div className="value">{stats.error_rate.toFixed(1)} per run</div>
          </div>
          {stats.total_tokens && (
            <div className="stat">
              <div className="label">Total Tokens</div>
              <div className="value">{(stats.total_tokens.total / 1000000).toFixed(2)}M</div>
            </div>
          )}
          {stats.runs_with_loop > 0 && (
            <div className="stat">
              <div className="label">With Loops</div>
              <div className="value">{stats.runs_with_loop}</div>
            </div>
          )}
        </div>
      )}

      {loading && (
        <div style={{ padding: 40, textAlign: "center" }}>
          <Spinner />
        </div>
      )}

      {!loading && runs.length === 0 && (
        <div className="card">
          <div className="muted" style={{ lineHeight: 1.6 }}>
            No runs yet. Agent runs are tracked locally as they complete. Start an agent
            from a task to see its run history here.
          </div>
        </div>
      )}

      {!loading && runs.length > 0 && (
        <div style={{ padding: "8px 0" }}>
          {runs.map((run) => {
            const isExpanded = expandedRun === run.id;
            const agentName = agentMap.get(run.agent_id) || run.agent_id;
            const projectName = run.project_name || "Task-wide";

            return (
              <div
                key={run.id}
                className="run-row"
                style={{
                  padding: "12px 16px",
                  borderBottom: "1px solid var(--divider)",
                  cursor: "pointer",
                  transition: "background 0.1s",
                }}
                onMouseEnter={(e) => {
                  e.currentTarget.style.background = "var(--hover-bg)";
                }}
                onMouseLeave={(e) => {
                  e.currentTarget.style.background = "";
                }}
                onClick={() => setExpandedRun(isExpanded ? null : run.id)}
              >
                <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
                  <div style={{ fontSize: 16 }}>{resultIcon(run.result)}</div>

                  <div style={{ flex: 1, minWidth: 0 }}>
                    <div style={{ fontSize: 14, fontWeight: 500 }}>
                      {agentName} · {run.task_name}
                    </div>
                    <div style={{ fontSize: 12, color: "var(--grey)", marginTop: 2 }}>
                      {projectName}
                      {run.branch && ` · ${run.branch}`} · {ago(run.ended_at, now)}
                    </div>
                  </div>

                  <div style={{ fontSize: 13, color: "var(--grey)", textAlign: "right" }}>
                    {formatDuration(run.duration_secs)}
                  </div>

                  {run.error_count > 0 && (
                    <div
                      style={{
                        fontSize: 11,
                        color: "var(--amber)",
                        background: "var(--amber-dim)",
                        padding: "2px 6px",
                        borderRadius: 3,
                      }}
                    >
                      {run.error_count} error{run.error_count === 1 ? "" : "s"}
                    </div>
                  )}

                  {run.loop_rounds !== null && (
                    <div
                      style={{
                        fontSize: 11,
                        color: "var(--blue)",
                        background: "var(--blue-dim)",
                        padding: "2px 6px",
                        borderRadius: 3,
                      }}
                    >
                      loop: {run.loop_rounds} round{run.loop_rounds === 1 ? "" : "s"}
                    </div>
                  )}
                </div>

                {isExpanded && (
                  <div
                    style={{
                      marginTop: 12,
                      paddingTop: 12,
                      borderTop: "1px solid var(--divider)",
                      fontSize: 13,
                      display: "grid",
                      gridTemplateColumns: "auto 1fr",
                      gap: "6px 12px",
                    }}
                  >
                    <div style={{ color: "var(--grey)" }}>Started:</div>
                    <div>{new Date(run.started_at).toLocaleString()}</div>

                    <div style={{ color: "var(--grey)" }}>Ended:</div>
                    <div>{new Date(run.ended_at).toLocaleString()}</div>

                    <div style={{ color: "var(--grey)" }}>Exit code:</div>
                    <div>{run.exit_code !== null ? run.exit_code : "—"}</div>

                    <div style={{ color: "var(--grey)" }}>Result:</div>
                    <div style={{ textTransform: "capitalize" }}>{run.result}</div>

                    {run.tokens && (
                      <>
                        <div style={{ color: "var(--grey)" }}>Tokens:</div>
                        <div>
                          {formatTokens(run.tokens)} (in: {(run.tokens.input / 1000).toFixed(1)}k, out:{" "}
                          {(run.tokens.output / 1000).toFixed(1)}k)
                        </div>
                      </>
                    )}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}

      {confirmClear && (
        <Confirm
          title="Clear all run history?"
          body="This will permanently delete all recorded agent runs. This action cannot be undone."
          confirmLabel="Clear"
          danger={true}
          onConfirm={() => void clearData()}
          onCancel={() => setConfirmClear(false)}
        />
      )}
    </div>
  );
}
