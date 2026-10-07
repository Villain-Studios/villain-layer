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
  /**
   * The active tab's session; null while the task has no tab. A new one is
   * another tab, or the tab made again: the panel watches it afresh.
   */
  tab: string | null;
  url: string;
  title: string;
  loading: boolean;
  /** Agents may use the page as it is now (BRW-3). */
  agents_may: boolean;
  last_action: AgentAction | null;
  /** Sites the task's agents asked for, waiting on the user (BRW-11). */
  requests: SiteRequest[];
  /** A dialog the page opened, waiting on an answer (BRW-13). */
  dialog: PageDialog | null;
  /** The user has taken the tab over, and agents wait (BRW-12). */
  held: boolean;
  /** Every tab, in order; the active one is what is shown (BRW-16). */
  tabs: TabInfo[];
  /** The agent using the tab, or that last did (BRW-15). */
  driver: Driver | null;
}

/** The agent using a task's tab, or that last did (BRW-15). */
export interface Driver {
  /** Its pane, whose state says whether it is still at work. */
  pane: string;
  /** "Claude Code". */
  agent: string;
  /** Its browser calls running now. */
  calls: number;
  /** When its last call began or ended, in milliseconds since the epoch. */
  last_at: number;
}

/** An alert, confirm or prompt the page opened. Headless Chrome draws none. */
export interface PageDialog {
  kind: "alert" | "confirm" | "prompt" | "beforeunload";
  message: string;
  default_prompt: string;
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

/** One of a task's tabs (BRW-16). */
export interface TabInfo {
  /** Chrome's id for the page. */
  id: string;
  /** Its title, else its site, else "New tab" (BRW-17). */
  name: string;
  url: string;
  active: boolean;
  loading: boolean;
}

/** A saved sign-in, without its password, which stays in the keychain (BRW-18). */
export interface SavedSignIn {
  id: string;
  /** A host, with a port or not: `localhost` alone is every port of it. */
  site: string;
  username: string;
}

/** What "Save sign-in" would save from the page (BRW-20): never the password. */
export interface SignInForm {
  site: string;
  /** The form's username, if it has one. */
  username: string | null;
}
