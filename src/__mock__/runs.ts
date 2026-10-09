/**
 * The run log (§22) for the mock harness: two weeks of made-up runs over the
 * busy world's tasks, the same every time, with one of each way a run can
 * end. The empty scenario has none.
 */
import { emit } from "@tauri-apps/api/event";
import type { Run, RunEnd, RunLoop } from "../lib/types-runs";

const api = { id: "p-api", name: "api" };
const web = { id: "p-web", name: "web" };
const TASKS = [
  { task_id: "t-login", task: "ACME-123 Fix login race", repos: [api, web] },
  { task_id: "t-audit", task: "ACME-130 Audit log page", repos: [web] },
  { task_id: "t-gone", task: "", repos: [api] },
  { task_id: "chat", task: "Chat", repos: [] },
];
const AGENTS = ["claude", "claude", "claude", "copilot", "gemini"];

function made(): Run[] {
  // The same runs at every load: a small linear congruential generator.
  let seed = 7;
  const next = () => (seed = (seed * 1103515245 + 12345) % 2 ** 31) / 2 ** 31;
  const now = Date.now();
  const runs: Run[] = [];
  for (let i = 0; i < 46; i++) {
    const ended = now - Math.floor(next() * 14 * 86_400_000) - 60_000;
    const took = Math.floor(60_000 + next() * 50 * 60_000);
    const t = TASKS[Math.floor(next() * TASKS.length)];
    const agent = AGENTS[Math.floor(next() * AGENTS.length)];
    const acp = agent === "claude" && next() < 0.6;
    const roll = next();
    const end: RunEnd = roll < 0.12 ? "failed" : roll < 0.3 ? "exited" : "stopped";
    let loop: RunLoop | null = null;
    if (end === "stopped" && next() < 0.45) {
      const passed = next() < 0.75;
      loop = passed ? { used: Math.floor(next() * 4), rounds: 5, end: "passed" } : { used: 5, rounds: 5, end: "gave_up" };
    }
    runs.push({
      id: `run-${i}`,
      agent,
      ...t,
      acp,
      started_at: new Date(ended - took).toISOString(),
      ended_at: new Date(ended).toISOString(),
      end,
      code: end === "stopped" ? 1 : end === "exited" ? 0 : 2,
      loop,
      // Claude Code says, over ACP or in its transcript; the others do not.
      tokens: agent === "claude"
        ? { input: Math.floor(next() * 4000), output: Math.floor(next() * 60_000), cached_read: Math.floor(next() * 3_000_000), cached_write: Math.floor(next() * 200_000) }
        : null,
    });
  }
  // A loop stopped by hand, and a run still on its loop when its agent ended.
  runs.push({ ...runs[0], id: "run-loop-stopped", end: "stopped", loop: { used: 2, rounds: 5, end: "stopped" }, ended_at: new Date(now - 90_000).toISOString(), started_at: new Date(now - 1_500_000).toISOString() });
  return runs.sort((a, b) => Date.parse(b.ended_at) - Date.parse(a.ended_at));
}

export function runAnswers(seeded: boolean) {
  let runs = seeded ? made() : [];
  return {
    list_runs: () => structuredClone(runs),
    clear_runs: () => {
      runs = [];
      void emit("runs:changed");
      return null;
    },
  };
}
