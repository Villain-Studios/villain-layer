/** The browser panel's types (§18), mirrored from `browser/` and `commands/browser.rs`. */
/** What an agent last did in a task's browser tab (BRW-10). */
export interface AgentAction {
  text: string;
  /** Where it clicked, in the page's CSS pixels. */
  x: number | null;
  y: number | null;
  /** Milliseconds since the epoch. */
  at: number;
}

/** A task's browser tab, as the panel shows it (§18). */
export interface BrowserView {
  /** A Chrome is installed to run. */
  chrome: boolean;
  /** The tab's session; null while the task has no tab. A new one is a new tab. */
  tab: string | null;
  url: string;
  title: string;
  loading: boolean;
  /** Agents may use the page as it is now (BRW-3). */
  agents_may: boolean;
  last_action: AgentAction | null;
  /** Sites the task's agents asked for, waiting on the user (BRW-11). */
  requests: SiteRequest[];
}

/** An agent asking to use a site in the browser (BRW-11). */
export interface SiteRequest {
  id: number;
  task: string;
  site: string;
  /** What the agent wants there, in its own words. */
  reason: string;
  at: number;
}

/** The user's own input in the panel. Coordinates are the page's CSS pixels. */
export type BrowserInput =
  | {
      kind: "mouse";
      type: "mousePressed" | "mouseReleased" | "mouseMoved";
      x: number;
      y: number;
      button: "left" | "right" | "middle" | "none";
      buttons: number;
      click_count: number;
      modifiers: number;
    }
  | { kind: "wheel"; x: number; y: number; dx: number; dy: number; modifiers: number }
  | {
      kind: "key";
      type: "keyDown" | "keyUp";
      key: string;
      code: string;
      key_code: number;
      text: string | null;
      modifiers: number;
      location: number;
    }
  | { kind: "text"; text: string };
