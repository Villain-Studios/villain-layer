import { useMemo, useState } from "react";
import { api } from "../lib/api";
import { CHAT_TASK_ID } from "../lib/derive";
import { goTo } from "../lib/goto";
import { useStore } from "../store";
import type { WaitingPane } from "../lib/types";
import { Confirm } from "./ui";

const NONE: WaitingPane[] = [];

/** The waiting panes of one task, or of the chats. */
function useWaiting(taskId: string): WaitingPane[] {
  const all = useStore((s) => s.waiting);
  return useMemo(() => {
    const mine = all.filter((w) => w.task_id === taskId);
    return mine.length ? mine : NONE;
  }, [all, taskId]);
}

/**
 * Reopen and Forget for a pane the last launch did not put back (PANE-7).
 * They used to be forgotten at launch, and a chat with a day's work in it
 * went with them: nothing in the app could open it again.
 */
function useActions(taskId: string) {
  const fail = useStore((s) => s.fail);
  const [busy, setBusy] = useState<string | null>(null);
  const [forgetting, setForgetting] = useState<WaitingPane | null>(null);
  const refresh = () => Promise.all([useStore.getState().refreshPanes(), useStore.getState().refreshWaiting()]);

  async function reopen(w: WaitingPane) {
    if (busy) return;
    setBusy(w.id);
    try {
      const pane = await api.reopenWaitingPane(w.id);
      await refresh();
      goTo(`pane:${taskId}:${pane.id}`);
    } catch (e) {
      fail(e);
      void refresh().catch(() => {});
    } finally {
      setBusy(null);
    }
  }

  async function forget(w: WaitingPane) {
    try {
      await api.forgetWaitingPane(w.id);
      await useStore.getState().refreshWaiting();
    } catch (e) {
      fail(e);
    }
  }

  const confirm = forgetting && (
    <Confirm
      title={forgetting.kind === "shell" ? "Forget this shell?" : "Forget this conversation?"}
      confirmLabel="Forget"
      body={
        forgetting.kind === "shell"
          ? "It will not be listed here again."
          : "It will not be listed here again. Its conversation stays in the agent's own history on disk."
      }
      onCancel={() => setForgetting(null)}
      onConfirm={async () => {
        await forget(forgetting);
        setForgetting(null);
      }}
    />
  );
  return { busy, reopen, ask: setForgetting, confirm };
}

/** Chats the last launch did not put back, under the open ones in the Chat view. */
export function WaitingChats() {
  const waiting = useWaiting(CHAT_TASK_ID);
  const { busy, reopen, ask, confirm } = useActions(CHAT_TASK_ID);
  if (waiting.length === 0) return null;
  return (
    <>
      <div className="chat-list-waiting" title="At most 12 panes come back at launch. These wait for you.">
        Not reopened
      </div>
      {waiting.map((w) => (
        <div key={w.id} className="chat-item waiting" onClick={() => void reopen(w)} title="Reopen this chat where it left off">
          <span className="dot" />
          <span className="chat-item-text">
            <span className="chat-item-title">{w.title ?? "Chat"}</span>
            <span className="chat-item-sub">{busy === w.id ? "Reopening…" : "not reopened at launch · click to reopen"}</span>
          </span>
          <span className="x" title="Forget this chat" onClick={(e) => { e.stopPropagation(); ask(w); }}>✕</span>
        </div>
      ))}
      {confirm}
    </>
  );
}

/** A task's panes the last launch did not put back, at the top of its Terminals tab. */
export function WaitingTaskPanes({ taskId }: { taskId: string }) {
  const waiting = useWaiting(taskId);
  const agents = useStore((s) => s.agents);
  const { busy, reopen, ask, confirm } = useActions(taskId);
  if (waiting.length === 0) return null;
  return (
    <div className="limit-banner waiting-banner">
      <span>
        {waiting.length === 1 ? "A pane was" : `${waiting.length} panes were`} not reopened at launch (12 at most):
      </span>
      {waiting.map((w) => (
        <span key={w.id} className="waiting-pane">
          <b>{w.kind === "shell" ? "Shell" : agents.find((a) => a.id === w.agent_id)?.name ?? w.agent_id ?? "Agent"}</b>
          {w.title && ` · ${w.title}`}
          <button className="btn btn-sm" disabled={busy !== null} onClick={() => void reopen(w)}>
            {busy === w.id ? "Reopening…" : "Reopen"}
          </button>
          <button className="btn btn-sm" title="Do not list it again" onClick={() => ask(w)}>Forget</button>
        </span>
      ))}
      {confirm}
    </div>
  );
}
