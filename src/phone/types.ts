/**
 * What the phone's server answers, mirrored by hand from
 * `src-tauri/src/phone/view.rs`, as `lib/types.ts` mirrors the commands.
 */

export type Activity = "working" | "asking" | "done" | "idle";

export interface PhonePane {
  id: string;
  name: string;
  kind: "agent" | "shell";
  agent: string | null;
  activity: Activity;
  since: string;
  notice: string | null;
  running: boolean;
  /** Why it needs you, by the dock count's rule; null when it does not. */
  waiting: string | null;
}

export interface Group {
  id: string;
  name: string;
  key: string | null;
  panes: PhonePane[];
}

export interface Overview {
  device: string;
  typing: boolean;
  groups: Group[];
}

/** A line of a pane's output stream (`routes.rs`, `output`). */
export type OutputLine =
  | { size: [number, number] }
  | { data: string; end: number; reset: boolean }
  | { exit: number | null }
  /** Not the server's: `follow` says so when the pane is closed (a 404). */
  | { gone: true };
