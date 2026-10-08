/**
 * A pretend loop (§21) for the mock harness: started, it waits, checks,
 * sends one failure back, checks again and passes, a step every second or
 * two, telling the window as the real one does (`loop:changed`). Enough to
 * look at every state of the bar; nothing like a real check's timing.
 *
 * `?loop=checking|held|passed|gave_up` seeds the selected task's
 * agents with a loop in that state.
 */
import { emit } from "@tauri-apps/api/event";
import type { LoopCheckRun, LoopPhase, LoopView, Project } from "../lib/types";

const loops = new Map<string, LoopView>();
const timers = new Map<string, number[]>();

const FAILED = [
  "$ bun run check",
  "guard: ok — 128 commands, 16 events, 263 files checked",
  "src/components/Login.test.tsx:",
  "✗ signs in with a saved password [12.40ms]",
  "  expected: \"Welcome back\"",
  "  received: \"Sign in\"",
  "",
  " 41 pass",
  " 1 fail",
].join("\n");

function run(round: number, passed: boolean): LoopCheckRun {
  return {
    at: new Date().toISOString(),
    round,
    results: [
      { repo: "web", command: "bun run check", passed, code: passed ? 0 : 1, secs: passed ? 41.2 : 39.8, output: passed ? "41 pass\n0 fail" : FAILED },
      { repo: "api", command: "cargo test", passed: true, code: 0, secs: 63.1, output: "test result: ok. 212 passed" },
    ],
  };
}

function view(pane: string, task: string, phase: LoopPhase): LoopView {
  return {
    pane_id: pane, task_id: task, phase, round: 0, rounds: 5, started_at: new Date().toISOString(),
    repos: ["web", "api"], checking: null, note: null, runs: [],
  };
}

function tell(pane: string) {
  void emit("loop:changed", pane);
}

function step(pane: string, after: number, change: (v: LoopView) => void) {
  const id = window.setTimeout(() => {
    const v = loops.get(pane);
    if (!v) return;
    change(v);
    tell(pane);
  }, after);
  timers.set(pane, [...(timers.get(pane) ?? []), id]);
}

/** The states a seeded loop can be shown in. */
export function seedLoop(pane: string, task: string, phase: string) {
  const v = view(pane, task, "waiting");
  if (phase === "checking") Object.assign(v, { phase, checking: "web", runs: [run(0, false)], round: 1 });
  if (phase === "held") {
    Object.assign(v, {
      phase, round: 1, runs: [run(0, false)],
      note: "The agent ended its turn without changing anything: it may be asking you something. The loop goes on after a turn that changes something.",
    });
  }
  if (phase === "passed") Object.assign(v, { phase, round: 1, runs: [run(1, true), run(0, false)], note: "The checks pass, after 1 round." });
  if (phase === "gave_up") {
    Object.assign(v, { phase, round: 5, runs: [5, 4, 3, 2, 1, 0].map((r) => run(r, false)), note: "The checks still fail after 5 rounds." });
  }
  loops.set(pane, v);
}

export function loopAnswers(projects: Project[]): Record<string, (a: Record<string, unknown>) => unknown> {
  return {
    loop_view: (a) => structuredClone(loops.get(a.paneId as string) ?? null),
    set_project_check: (a) => {
      const p = projects.find((x) => x.id === a.projectId);
      if (p) p.check = (a.command as string | null) ?? null;
      return null;
    },
    start_loop: (a) => {
      const pane = a.paneId as string;
      const old = loops.get(pane);
      if (old && !["passed", "gave_up", "stopped"].includes(old.phase)) throw "This agent is on a loop already.";
      const v = { ...view(pane, "t-login", "waiting"), rounds: a.rounds as number };
      loops.set(pane, v);
      step(pane, 1200, (v) => Object.assign(v, { phase: "checking", checking: "web" }));
      step(pane, 2400, (v) => Object.assign(v, { checking: "api" }));
      step(pane, 3200, (v) => Object.assign(v, { phase: "waiting", checking: null, round: 1, runs: [run(0, false)] }));
      step(pane, 6000, (v) => Object.assign(v, { phase: "checking", checking: "web" }));
      step(pane, 7500, (v) => Object.assign(v, {
        phase: "passed", checking: null, runs: [run(1, true), ...v.runs], note: "The checks pass, after 1 round.",
      }));
      setTimeout(() => tell(pane), 0);
      return structuredClone(v);
    },
    stop_loop: (a) => {
      const pane = a.paneId as string;
      for (const id of timers.get(pane) ?? []) clearTimeout(id);
      timers.delete(pane);
      const v = loops.get(pane);
      if (v) Object.assign(v, { phase: "stopped", checking: null, note: "Stopped." });
      tell(pane);
      return null;
    },
  };
}
