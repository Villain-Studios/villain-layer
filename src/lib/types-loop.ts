/**
 * An agent on a loop (§21), as `loops.rs` keeps it, mirrored here. Split
 * from `types.ts` at its size ceiling; import from there.
 */

/**
 * `waiting`: the agent is in a turn, checked at its end. `queued`: other
 * loops' checks are running (LOOP-5). `held`: waiting on you, after a turn
 * that changed nothing or hit a usage limit (LOOP-4). The last three are
 * how it ended.
 */
export type LoopPhase = "waiting" | "queued" | "checking" | "held" | "passed" | "gave_up" | "stopped";

export interface LoopCheckResult {
  /** The repository's folder in the task. */
  repo: string;
  command: string;
  passed: boolean;
  code: number | null;
  secs: number;
  /** The end of its output, as the agent is sent it (LOOP-6). */
  output: string;
}

export interface LoopCheckRun {
  at: string;
  /** Rounds used before it: 0 for the first check. */
  round: number;
  results: LoopCheckResult[];
}

export interface LoopView {
  pane_id: string;
  task_id: string;
  phase: LoopPhase;
  /** Rounds used: failures sent back to the agent. */
  round: number;
  rounds: number;
  started_at: string;
  /** The repositories it checks, by folder. */
  repos: string[];
  /** The one being checked now. */
  checking: string | null;
  /** Why it waits, or how it ended. */
  note: string | null;
  /** Newest first. */
  runs: LoopCheckRun[];
}
