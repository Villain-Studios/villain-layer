import { useStore } from "../store";
import { goTo } from "../lib/goto";
import { CHAT_TASK_ID, paneName, paneState } from "../lib/derive";
import type { Task } from "../lib/types";

/**
 * A task a chat's agent created, and which that agent is working on from the
 * chat (CHAT-3). Its pane is never one of the task's, so without this the
 * task read "No panes yet" while the work was going on somewhere else.
 *
 * Found by the chat's room folder: the pane gets a new id at every launch.
 */
export function ChatLink({ task }: { task: Task }) {
  const pane = useStore((s) =>
    task.chat
      ? s.panes.find((p) => p.task_id === CHAT_TASK_ID && p.cwd === task.chat)
      : undefined,
  );
  if (!pane) return null;
  const state = paneState(pane);
  return (
    <div className="limit-banner chat-link-banner">
      <span className={`dot ${state.dot}`} title={state.label} />
      <span title="Its agent works on this task from the chat, so its pane is there, not here">
        Started from the chat <b>{paneName(pane)}</b> · {state.label}
      </span>
      <div className="spacer" />
      <button className="btn btn-sm" onClick={() => goTo(`pane:${CHAT_TASK_ID}:${pane.id}`)}>
        Open chat
      </button>
    </div>
  );
}
