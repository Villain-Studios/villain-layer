/**
 * An agent over ACP (§20): its conversation as the backend keeps it
 * (`acp/conversation.rs`), mirrored here. Split from `types.ts` at its size
 * ceiling; import from there.
 */

export type AcpToolContent =
  | { type: "text"; text: string }
  | { type: "diff"; path: string; old: string | null; new: string };

export interface AcpChoice {
  id: string;
  name: string;
  /** allow_once, allow_always, reject_once or reject_always. */
  kind: string;
}

/** One entry of the conversation. `index` stays the same as older ones go. */
export type AcpEntry = { index: number; rev: number } & (
  /** `queued` while it waits for the turn before it; `dropped` if Stop ended that turn first. */
  | { kind: "user"; text: string; queued: boolean; dropped: boolean }
  | { kind: "agent"; text: string }
  | { kind: "thought"; text: string }
  | {
      kind: "tool";
      id: string;
      title: string;
      /** read, edit, delete, move, search, execute, think, fetch, switch_mode, other. */
      tool: string;
      /** pending, in_progress, completed or failed. */
      status: string;
      content: AcpToolContent[];
      locations: string[];
    }
  | { kind: "plan"; steps: { content: string; status: string }[] }
  /** A permission question (ACP-5); `answer` is the option chosen, or "cancelled". */
  | { kind: "question"; title: string; options: AcpChoice[]; answer: string | null }
  /** Something the app says: an error, how to sign in. */
  | { kind: "note"; text: string; error: boolean }
);

/** A setting the agent offers: its mode, its model (ACP-12). */
export interface AcpSetting {
  id: string;
  name: string;
  category: string | null;
  current: string;
  options: { value: string; name: string; description: string | null }[];
}

export interface AcpCommand {
  name: string;
  description: string;
  hint: string | null;
}

/** What changed in a conversation since the revision asked from (ACP-7). */
export interface AcpView {
  rev: number;
  /** The first index still kept: drop anything below it. */
  base: number;
  len: number;
  /** `entries` is everything kept: replace, do not merge. */
  reset: boolean;
  entries: AcpEntry[];
  /** The agent's name and version, once it has said. */
  agent: string | null;
  /** The conversation is open and takes prompts. */
  ready: boolean;
  /** A turn is running. */
  busy: boolean;
  /** Prompts waiting for the running turn to end. */
  queued: number;
  settings: AcpSetting[];
  commands: AcpCommand[];
  usage: { used: number; size: number; cost: string | null } | null;
  exited: boolean;
}
