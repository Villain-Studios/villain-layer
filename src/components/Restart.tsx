import { useRef, useState } from "react";
import { api } from "../lib/api";
import { canRestart, paneName } from "../lib/derive";
import { useStore } from "../store";
import type { PaneInfo } from "../lib/types";
import { ReloadIcon } from "./icons";

/**
 * Restart an agent on its own conversation (PANE-14), and select it once it
 * is back. Stopping waits up to five seconds for the agent to save, so what
 * is restarting is shown as such until then.
 */
export function useRestart(select: (paneId: string) => void) {
  const fail = useStore((s) => s.fail);
  const [restarting, setRestarting] = useState<ReadonlySet<string>>(new Set());
  // A ref as well as state: two clicks in one frame both saw the state empty.
  const busy = useRef(new Set<string>());

  async function restart(p: PaneInfo) {
    if (busy.current.has(p.id)) return;
    busy.current.add(p.id);
    setRestarting(new Set(busy.current));
    const { refreshPanes, refreshWaiting } = useStore.getState();
    try {
      const pane = await api.restartPane(p.id);
      await refreshPanes();
      select(pane.id);
    } catch (e) {
      fail(e);
      // Stopped and then not started, it waits to be reopened (PANE-7).
      void Promise.all([refreshPanes(), refreshWaiting()]).catch(() => {});
    } finally {
      busy.current.delete(p.id);
      setRestarting(new Set(busy.current));
    }
  }
  return { restarting, restart };
}

/** Restart, on a pane's tab and on a chat's entry in the list. */
export function RestartButton({ pane, onRestart }: { pane: PaneInfo; onRestart: (p: PaneInfo) => void }) {
  return (
    <span
      className="x"
      title={pane.running
        ? "Restart on the same conversation, to run a CLI that has updated itself"
        : "Resume the conversation"}
      onClick={(e) => { e.stopPropagation(); onRestart(pane); }}
    >
      <ReloadIcon size={13} />
    </span>
  );
}

/**
 * Above an agent that has exited: pick its conversation back up. Its CLI
 * said how ("Resume this session with: claude --resume …"), and nothing in
 * the app could.
 */
export function PaneExited({ pane, busy, onResume }: {
  pane: PaneInfo | undefined;
  busy: boolean;
  onResume: (p: PaneInfo) => void;
}) {
  const agents = useStore((s) => s.agents);
  if (!pane || pane.running || !canRestart(pane, agents)) return null;
  return (
    <div className="limit-banner">
      <span>
        <b>{paneName(pane)}</b> has exited{pane.exit_code ? ` with ${pane.exit_code}` : ""}.
        Resume picks its conversation up where it stopped.
      </span>
      <div className="spacer" />
      <button className="btn btn-sm btn-primary" disabled={busy} onClick={() => onResume(pane)}>
        {busy ? "Resuming…" : "Resume"}
      </button>
    </div>
  );
}
