import { useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "../lib/api";
import {
  durationMs,
  filterRuns,
  formatDuration,
  formatTokens,
  resultOf,
  runChoices,
  runDays,
  runStats,
  totalTokens,
  type RunResult,
} from "../lib/runs";
import { ago } from "../lib/time";
import type { Run } from "../lib/types-runs";
import { useNow, useStore } from "../store";
import { Confirm, SidebarToggle, Spinner } from "./ui";

/** What `runs.rs` keeps; said once the log is that full (RUN-5). */
const CAP = 1000;
/** Days the chart covers (RUN-4). */
const DAYS = 14;

const RESULT: Record<RunResult, { mark: string; label: string }> = {
  passed: { mark: "✓", label: "Its loop passed" },
  gave_up: { mark: "✗", label: "Its loop gave up" },
  failed: { mark: "✗", label: "Failed" },
  stopped: { mark: "■", label: "Stopped" },
  exited: { mark: "●", label: "Exited on its own" },
};

/** Every agent's run, newest first, with what they add up to (§22). */
export function RunsView() {
  const agents = useStore((s) => s.agents);
  const fail = useStore((s) => s.fail);
  const now = useNow(30_000);
  const [runs, setRuns] = useState<Run[] | null>(null);
  const [agent, setAgent] = useState<string | null>(null);
  const [repo, setRepo] = useState<string | null>(null);
  const [open, setOpen] = useState<string | null>(null);
  const [clearing, setClearing] = useState(false);

  useEffect(() => {
    let current = true;
    let asked = 0;
    const load = () => {
      // Agents ending together ask together; only the last answer counts.
      const mine = ++asked;
      api.listRuns().then((r) => { if (current && mine === asked) setRuns(r); }).catch(fail);
    };
    load();
    const p = listen("runs:changed", load);
    return () => {
      current = false;
      void p.then((un) => un());
    };
  }, [fail]);

  const all = useMemo(() => runs ?? [], [runs]);
  const choices = useMemo(() => runChoices(all), [all]);
  const shown = useMemo(() => filterRuns(all, { agent, repo }), [all, agent, repo]);
  const stats = useMemo(() => runStats(shown), [shown]);
  const days = useMemo(() => runDays(shown, DAYS, now), [shown, now]);
  const peak = Math.max(1, ...days.map((d) => d.runs));
  const agentName = (id: string) => agents.find((a) => a.id === id)?.name ?? (id || "an agent");

  return (
    <div className="wide runs">
      <div className="wide-head">
        <SidebarToggle />
        <h2>Runs</h2>
        <span className="sub">
          {all.length >= CAP ? `the newest ${CAP} are kept` : "kept on this Mac only"}
        </span>
        <div className="spacer" />
        {all.length > 0 && (
          <>
            <select value={agent ?? ""} onChange={(e) => setAgent(e.target.value || null)} aria-label="Agent">
              <option value="">Every agent</option>
              {choices.agents.map((a) => <option key={a} value={a}>{agentName(a)}</option>)}
            </select>
            <select value={repo ?? ""} onChange={(e) => setRepo(e.target.value || null)} aria-label="Repository">
              <option value="">Every repository</option>
              {choices.repos.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
            </select>
            <button type="button" className="btn btn-sm btn-danger" onClick={() => setClearing(true)}>
              Clear…
            </button>
          </>
        )}
      </div>

      {runs === null ? (
        <div className="empty"><Spinner /></div>
      ) : all.length === 0 ? (
        <div className="empty">
          <h2>No runs yet</h2>
          <p>
            Each agent is recorded here when it ends: how long it ran, how it ended, the loop it was on, and the
            tokens it used where it says. Nothing leaves this Mac.
          </p>
        </div>
      ) : shown.length === 0 ? (
        <div className="empty"><p>No run matches these filters.</p></div>
      ) : (
        <>
          <div className="run-stats">
            <Stat label="Runs" value={String(stats.runs)} />
            <Stat
              label="Went wrong"
              value={`${Math.round(stats.wrongRate * 100)}%`}
              note={`${stats.wrong} of ${stats.runs}: failed, or a loop gave up`}
              bad={stats.wrong > 0}
            />
            <Stat label="Average run" value={formatDuration(stats.avgMs)} />
            <Stat
              label="On a loop"
              value={String(stats.looped)}
              note={stats.looped ? `${stats.rounds} ${stats.rounds === 1 ? "round" : "rounds"} sent back` : undefined}
            />
            <Stat
              label="Tokens"
              value={stats.tokens ? formatTokens(totalTokens(stats.tokens)) : "—"}
              note={stats.tokens ? `said by ${stats.withTokens} of ${stats.runs}` : "only Claude Code and agents over ACP say"}
            />
          </div>

          <div className="run-days" role="img" aria-label={`Runs on each of the last ${DAYS} days, and those that went wrong`}>
            {days.map((d) => (
              <div key={d.day} className="run-day" title={`${d.day}: ${d.runs} ${d.runs === 1 ? "run" : "runs"}, ${d.wrong} went wrong`}>
                <div className="bar" style={{ height: `${(d.runs / peak) * 100}%` }}>
                  <div className="wrong" style={{ height: d.runs ? `${(d.wrong / d.runs) * 100}%` : 0 }} />
                </div>
              </div>
            ))}
          </div>
          <div className="run-days-axis">
            <span>{DAYS} days ago</span>
            <span>today</span>
          </div>

          <div className="run-list">
            {shown.map((r) => (
              <RunRow
                key={r.id}
                run={r}
                agentName={agentName(r.agent)}
                open={open === r.id}
                onToggle={() => setOpen(open === r.id ? null : r.id)}
                now={now}
              />
            ))}
          </div>
        </>
      )}

      {clearing && (
        <Confirm
          title="Clear the run log?"
          body="Every run kept here is forgotten. Agents, tasks and their work are not touched."
          confirmLabel="Clear"
          onConfirm={() =>
            api.clearRuns().then(() => {
              setAgent(null);
              setRepo(null);
            }).catch(fail)
          }
          onCancel={() => setClearing(false)}
        />
      )}
    </div>
  );
}

function Stat({ label, value, note, bad }: { label: string; value: string; note?: string; bad?: boolean }) {
  return (
    <div className="run-stat">
      <div className="label">{label}</div>
      <div className={`value${bad ? " bad" : ""}`}>{value}</div>
      {note && <div className="note">{note}</div>}
    </div>
  );
}

function RunRow({ run, agentName, open, onToggle, now }: {
  run: Run;
  agentName: string;
  open: boolean;
  onToggle: () => void;
  now: number;
}) {
  const result = resultOf(run);
  return (
    <div className={`run-row ${result}`}>
      <button type="button" className="run-line" onClick={onToggle} aria-expanded={open}>
        <span className="run-mark" title={RESULT[result].label}>{RESULT[result].mark}</span>
        <span className="run-what"><b>{agentName}</b> · {run.task || "a task since removed"}</span>
        <span className="chips">
          {run.repos.map((p) => <span key={p.id} className="chip">{p.name}</span>)}
          {run.loop && <span className="chip">{run.loop.used}/{run.loop.rounds} rounds</span>}
        </span>
        <span className="spacer" />
        <span className="run-took">{formatDuration(durationMs(run))}</span>
        <span className="run-when">{ago(run.ended_at, now)}</span>
      </button>
      {open && (
        <dl className="run-detail">
          <dt>Ended</dt>
          <dd>{ending(run)}</dd>
          {run.loop && (
            <>
              <dt>Loop</dt>
              <dd>
                {run.loop.end === "passed" ? "passed" : run.loop.end === "gave_up" ? "gave up" : "stopped"},{" "}
                {run.loop.used} of {run.loop.rounds} rounds used
              </dd>
            </>
          )}
          <dt>Ran</dt>
          <dd>
            {new Date(run.started_at).toLocaleString()} – {new Date(run.ended_at).toLocaleTimeString()},{" "}
            {run.acp ? "as a conversation (ACP)" : "in a terminal"}
          </dd>
          <dt>Tokens</dt>
          <dd>
            {run.tokens
              ? `${formatTokens(run.tokens.input)} in · ${formatTokens(run.tokens.output)} out · ${formatTokens(run.tokens.cached_read)} read from cache · ${formatTokens(run.tokens.cached_write)} written to cache`
              : run.acp
                ? "not said: this agent puts no usage on its answers"
                : "not said: of the agents in a terminal, only Claude Code does"}
          </dd>
        </dl>
      )}
    </div>
  );
}

function ending(run: Run): string {
  switch (run.end) {
    case "stopped":
      return "stopped: by you, a restart, its task closing or the app quitting";
    case "exited":
      return "on its own, exit code 0";
    case "failed":
      return run.code === null ? "failed, with no exit code" : `failed, exit code ${run.code}`;
  }
}
