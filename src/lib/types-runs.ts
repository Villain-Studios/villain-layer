/**
 * The run log (§22), as `runs.rs` keeps it, mirrored here. Imported from
 * this file itself: `types.ts` is at its size ceiling.
 */

/** How the agent's process ended (RUN-2): asked to, or on its own with exit 0, or with any other. */
export type RunEnd = "stopped" | "exited" | "failed";

export interface RunLoop {
  /** Failures sent back to the agent. */
  used: number;
  /** The most it was allowed. */
  rounds: number;
  /** `stopped`: by you, or still going when the agent ended. */
  end: "passed" | "gave_up" | "stopped";
}

/** What its finished turns used, where the agent says (RUN-4). */
export interface RunTokens {
  input: number;
  output: number;
  cached_read: number;
  cached_write: number;
}

export interface Run {
  /** The pane's id. */
  id: string;
  agent: string;
  task_id: string;
  /** The task's name as it was; empty when it had already gone. */
  task: string;
  /** Its own repository, or every one of its task's at the task's root. */
  repos: { id: string; name: string }[];
  acp: boolean;
  started_at: string;
  ended_at: string;
  end: RunEnd;
  code: number | null;
  loop: RunLoop | null;
  tokens: RunTokens | null;
  /** Turns it finished. */
  turns: number;
  /** Times it stopped on something only you could answer (RUN-7). */
  asks: number;
  /** How long those waited on you. */
  waited_secs: number;
  /** It hit its plan's usage limit. */
  limited: boolean;
  /** Its tool calls, where the agent says: Claude Code's and Copilot's hooks, or ACP. */
  tools: { calls: number; failed: number } | null;
}
