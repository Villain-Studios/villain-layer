import type { PaneInfo } from "../lib/types";

/**
 * What the open pane's process is held up on, above its terminal: a folder
 * trust question to answer there, or a usage limit to hand the work off from.
 */
export function PaneNotice({ pane: p, onHandoff }: { pane: PaneInfo | undefined; onHandoff: (p: PaneInfo) => void }) {
  if (!p?.notice) return null;
  if (p.notice === "trust_prompt") {
    return (
      <div className="limit-banner">
        <span>
          <b>{p.title}</b> is asking whether to trust this folder — answer it in
          the terminal below. Every task gets its own worktree, so this is asked
          once per task, and nothing runs until it is answered.
        </span>
      </div>
    );
  }
  return (
    <div className="limit-banner">
      <span>
        <b>{p.title}</b> looks out of budget — it reported hitting a usage limit.
      </span>
      <div className="spacer" />
      <button className="btn btn-sm btn-primary" onClick={() => onHandoff(p)}>
        Hand off to another agent…
      </button>
    </div>
  );
}
