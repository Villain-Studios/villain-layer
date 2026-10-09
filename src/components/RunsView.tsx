import { useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "../lib/api";
import {
  byAgent,
  filterRuns,
  formatDuration,
  formatTokens,
  runChoices,
  runDays,
  runStats,
  topSpend,
  totalTokens,
} from "../lib/runs";
import type { Run } from "../lib/types-runs";
import { useNow, useStore } from "../store";
import { RunBreakdown } from "./runs/RunBreakdown";
import { RunChart } from "./runs/RunChart";
import { RunRow } from "./runs/RunRow";
import { Confirm, SidebarToggle, Spinner } from "./ui";

/** What `runs.rs` keeps; said once the log is that full (RUN-5). */
const CAP = 1000;
/** Days the chart covers (RUN-4). */
const DAYS = 14;
/** Tasks and repositories listed under where the time went (RUN-8). */
const TOP = 5;

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
  const perAgent = useMemo(() => byAgent(shown), [shown]);
  const tasks = useMemo(() => topSpend(shown, "task", TOP), [shown]);
  const repos = useMemo(() => topSpend(shown, "repo", TOP), [shown]);
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
            Each agent is recorded here when it ends: how long it ran, how it ended, how often it needed you, its
            tool calls, and the tokens it used where it says. Nothing leaves this Mac.
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
            <Stat
              label="Typical run"
              value={formatDuration(stats.medianMs)}
              note={`the slowest tenth over ${formatDuration(stats.p90Ms)}`}
            />
            <Stat
              label="Waited on you"
              value={formatDuration(stats.waitedSecs * 1000)}
              note={
                `asked ${stats.asks} ${stats.asks === 1 ? "time" : "times"}` +
                (stats.limited ? ` · ${stats.limited} hit a usage limit` : "")
              }
            />
            <Stat
              label="Tokens"
              value={stats.tokens ? formatTokens(totalTokens(stats.tokens)) : "—"}
              note={
                stats.tokens
                  ? `${formatTokens(Math.round(totalTokens(stats.tokens) / stats.withTokens))} a run, said by ${stats.withTokens} of ${stats.runs}`
                  : "only Claude Code and agents over ACP say"
              }
            />
            <Stat
              label="Failed tool calls"
              value={stats.tools?.calls ? `${Math.round((stats.tools.failed / stats.tools.calls) * 100)}%` : "—"}
              note={stats.tools ? `${stats.tools.failed} of ${stats.tools.calls}` : "only Claude Code, Copilot and ACP say"}
            />
            {stats.looped > 0 && (
              <Stat
                label="On a loop"
                value={String(stats.looped)}
                note={`${stats.rounds} ${stats.rounds === 1 ? "round" : "rounds"} sent back`}
              />
            )}
          </div>

          <RunChart days={days} />
          <RunBreakdown agents={perAgent} tasks={tasks} repos={repos} agentName={agentName} />

          <h3 className="run-list-title">Every run</h3>
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
