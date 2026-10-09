/**
 * Reading the run log (§22): what a run came to, and the sums the Runs view
 * shows over the runs its filters leave (RUN-3, RUN-4). Pure, so they are
 * tested on their own.
 */
import type { Run, RunTokens } from "./types-runs";

/**
 * What a run came to (RUN-2). A loop's verdict says more than the process
 * ending, which is usually you stopping it once the loop is done; a process
 * that failed says most of all.
 */
export type RunResult = "passed" | "gave_up" | "failed" | "stopped" | "exited";

export function resultOf(run: Run): RunResult {
  if (run.end === "failed") return "failed";
  if (run.loop?.end === "passed") return "passed";
  if (run.loop?.end === "gave_up") return "gave_up";
  return run.end;
}

/** It went wrong: the agent failed, or its loop ran out of rounds with checks still failing. */
export function wentWrong(run: Run): boolean {
  const r = resultOf(run);
  return r === "failed" || r === "gave_up";
}

export function durationMs(run: Run): number {
  return Math.max(0, Date.parse(run.ended_at) - Date.parse(run.started_at)) || 0;
}

export interface RunFilter {
  agent: string | null;
  repo: string | null;
}

/** A run counts for a repository it could have changed: its own, or any of its task's. */
export function filterRuns(runs: Run[], f: RunFilter): Run[] {
  return runs.filter((r) => (!f.agent || r.agent === f.agent) && (!f.repo || r.repos.some((p) => p.id === f.repo)));
}

export interface RunStats {
  runs: number;
  wrong: number;
  /** Of `runs`, 0 with none. */
  wrongRate: number;
  /**
   * The typical run, and the slowest tenth's. Not the average: one agent
   * left open overnight made the average of a day's runs hours long.
   */
  medianMs: number;
  p90Ms: number;
  /** Times agents stopped on something only you could answer, and how long they waited (RUN-7). */
  asks: number;
  waitedSecs: number;
  /** Runs that hit a usage limit. */
  limited: number;
  /** Runs that were on a loop, and the rounds they used between them. */
  looped: number;
  rounds: number;
  /** Summed over the runs that said, and how many did. */
  tokens: RunTokens | null;
  withTokens: number;
  /** Tool calls over the runs that said, and how many did. */
  tools: { calls: number; failed: number } | null;
  withTools: number;
}

/** The value `p` of the way up `values` (0.5 the median), 0 for none. */
export function percentile(values: number[], p: number): number {
  if (!values.length) return 0;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.max(0, Math.ceil(p * sorted.length) - 1))];
}

function addTokens(into: RunTokens, t: RunTokens) {
  into.input += t.input;
  into.output += t.output;
  into.cached_read += t.cached_read;
  into.cached_write += t.cached_write;
}

export function runStats(runs: Run[]): RunStats {
  let wrong = 0, asks = 0, waitedSecs = 0, limited = 0, looped = 0, rounds = 0, withTokens = 0, withTools = 0;
  const tokens: RunTokens = { input: 0, output: 0, cached_read: 0, cached_write: 0 };
  const tools = { calls: 0, failed: 0 };
  for (const r of runs) {
    if (wentWrong(r)) wrong++;
    asks += r.asks;
    waitedSecs += r.waited_secs;
    if (r.limited) limited++;
    if (r.loop) {
      looped++;
      rounds += r.loop.used;
    }
    if (r.tokens) {
      withTokens++;
      addTokens(tokens, r.tokens);
    }
    if (r.tools) {
      withTools++;
      tools.calls += r.tools.calls;
      tools.failed += r.tools.failed;
    }
  }
  const n = runs.length;
  const lengths = runs.map(durationMs);
  return {
    runs: n,
    wrong,
    wrongRate: n ? wrong / n : 0,
    medianMs: percentile(lengths, 0.5),
    p90Ms: percentile(lengths, 0.9),
    asks,
    waitedSecs,
    limited,
    looped,
    rounds,
    tokens: withTokens ? tokens : null,
    withTokens,
    tools: withTools ? tools : null,
    withTools,
  };
}

export function totalTokens(t: RunTokens): number {
  return t.input + t.output + t.cached_read + t.cached_write;
}

/** One day of the chart, by the Mac's own calendar. */
export interface RunDay {
  /** `YYYY-MM-DD`. */
  day: string;
  runs: number;
  wrong: number;
  tokens: number;
  waitedSecs: number;
}

function dayOf(t: number): string {
  const d = new Date(t);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

/** The last `days` days up to today, oldest first, with the runs that ended on each (RUN-4). */
export function runDays(runs: Run[], days: number, now: number): RunDay[] {
  const out: RunDay[] = [];
  const at = new Map<string, RunDay>();
  // Midday, so a day is still a day either side of a clock change.
  const today = new Date(now);
  today.setHours(12, 0, 0, 0);
  for (let i = days - 1; i >= 0; i--) {
    const d = new Date(today);
    d.setDate(today.getDate() - i);
    const day: RunDay = { day: dayOf(d.getTime()), runs: 0, wrong: 0, tokens: 0, waitedSecs: 0 };
    at.set(day.day, day);
    out.push(day);
  }
  for (const r of runs) {
    const day = at.get(dayOf(Date.parse(r.ended_at)));
    if (!day) continue;
    day.runs++;
    if (wentWrong(r)) day.wrong++;
    if (r.tokens) day.tokens += totalTokens(r.tokens);
    day.waitedSecs += r.waited_secs;
  }
  return out;
}

/** One agent's runs, side by side with the others' (RUN-8). */
export interface AgentRow {
  agent: string;
  stats: RunStats;
}

/** A row per agent, the busiest first. */
export function byAgent(runs: Run[]): AgentRow[] {
  const of = new Map<string, Run[]>();
  for (const r of runs) of.set(r.agent, [...(of.get(r.agent) ?? []), r]);
  return [...of].map(([agent, rs]) => ({ agent, stats: runStats(rs) })).sort((a, b) => b.stats.runs - a.stats.runs);
}

/** Where agents' time and tokens went: a task or a repository (RUN-8). */
export interface Spend {
  id: string;
  name: string;
  runs: number;
  ms: number;
  tokens: number;
}

/**
 * The tasks, or repositories, agents spent longest on, `n` of them. A run
 * at a task's root counts in full for each of its repositories, since it
 * could have changed any of them.
 */
export function topSpend(runs: Run[], by: "task" | "repo", n: number): Spend[] {
  const at = new Map<string, Spend>();
  for (const r of runs) {
    const places = by === "task" ? [{ id: r.task_id, name: r.task || "a task since removed" }] : r.repos;
    for (const p of places) {
      // Newest first, so the first name seen is the latest.
      const s = at.get(p.id) ?? { id: p.id, name: p.name || p.id, runs: 0, ms: 0, tokens: 0 };
      s.runs++;
      s.ms += durationMs(r);
      if (r.tokens) s.tokens += totalTokens(r.tokens);
      at.set(p.id, s);
    }
  }
  return [...at.values()].sort((a, b) => b.ms - a.ms).slice(0, n);
}

/** The filters' choices: every agent and repository the log has seen, the latest name for each. */
export function runChoices(runs: Run[]): { agents: string[]; repos: { id: string; name: string }[] } {
  const agents = new Set<string>();
  const repos = new Map<string, string>();
  for (const r of runs) {
    if (r.agent) agents.add(r.agent);
    // Newest first, so the first name seen is the latest.
    for (const p of r.repos) if (!repos.has(p.id)) repos.set(p.id, p.name || p.id);
  }
  return {
    agents: [...agents].sort(),
    repos: [...repos].map(([id, name]) => ({ id, name })).sort((a, b) => a.name.localeCompare(b.name)),
  };
}

/** "45s", "3m 12s", "1h 5m". */
export function formatDuration(ms: number): string {
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

/** "950", "12.3k", "4.5M". */
export function formatTokens(n: number): string {
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(1)}k`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}
