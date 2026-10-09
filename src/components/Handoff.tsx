import { useRef, useState } from "react";
import { api } from "../lib/api";
import { markStopping, useStore } from "../store";
import type { PaneInfo, TaskView } from "../lib/types";
import { Field, Modal, Spinner } from "./ui";

/**
 * Hand a pane's work to another agent (PANE-9): what opens the dialog, and
 * the dialog itself. The new pane is selected once it has started.
 */
export function useHandoff(task: TaskView, setActive: (paneId: string) => void) {
  const agents = useStore((s) => s.agents);
  const refreshPanes = useStore((s) => s.refreshPanes);
  const fail = useStore((s) => s.fail);
  const installed = agents.filter((a) => a.installed);
  const [handoff, setHandoff] = useState<{ from: PaneInfo; agentId: string } | null>(null);
  const [handoffPrompt, setHandoffPrompt] = useState("");
  const [handoffLoading, setHandoffLoading] = useState(false);
  const [handoffBusy, setHandoffBusy] = useState(false);
  const [stopOld, setStopOld] = useState(true);
  /** Which handoff briefing request is the current one. */
  const handoffAsk = useRef(0);

  /// No CLI can resume another's session, so what moves is the work: the
  /// ticket, the diff, and the outgoing agent's terminal tail.
  function startHandoff(from: PaneInfo) {
    const target = installed.find((a) => a.id !== from.agent_id) ?? installed[0];
    if (!target) {
      fail("No other agent CLI is installed to hand off to.");
      return;
    }
    setHandoff({ from, agentId: target.id });
    setHandoffPrompt("");
    setHandoffLoading(true);
    const asked = ++handoffAsk.current;
    // Only the latest ask lands: a slow briefing for one pane arriving after
    // the dialog was reopened for another replaced what was on screen.
    api.handoffPrompt(from.id)
      .then((p) => { if (asked === handoffAsk.current) setHandoffPrompt(p); })
      .catch((e) => { if (asked === handoffAsk.current) fail(e); })
      .finally(() => { if (asked === handoffAsk.current) setHandoffLoading(false); });
  }

  /// Spawns the new agent, then spends the grace period stopping the old one.
  /// The modal stays up for both, or the press looks like it did nothing while
  /// the outgoing agent saves.
  async function runHandoff() {
    if (!handoff) return;
    setHandoffBusy(true);
    try {
      const pane = await api.spawnAgent(
        task.id,
        handoff.agentId,
        handoff.from.checkout_id,
        handoffPrompt.trim() || null,
      );
      if (stopOld) {
        markStopping(handoff.from.id);
        await api.killPane(handoff.from.id).catch(() => {});
      }
      setHandoff(null);
      setHandoffPrompt("");
      await refreshPanes();
      setActive(pane.id);
    } catch (e) {
      fail(e);
    } finally {
      // Cleared here, where it was set. It lived in launchAgent's finally,
      // so after one handoff the dialog opened already busy, and after a
      // failed one it could not be closed at all — Cancel disabled, and
      // Escape, ✕ and the backdrop all ignored while busy.
      setHandoffBusy(false);
    }
  }

  const dialog = handoff && (
    <Modal
      title={`Hand off from ${handoff.from.title}`}
      wide
      onClose={() => {
        if (handoffBusy) return;
        setHandoff(null);
        setHandoffPrompt("");
      }}
      footer={
        <>
          <button
            className="btn"
            disabled={handoffBusy}
            onClick={() => { setHandoff(null); setHandoffPrompt(""); }}
          >
            Cancel
          </button>
          <button
            className="btn btn-primary"
            disabled={handoffBusy || handoffLoading || !handoffPrompt.trim()}
            onClick={() => void runHandoff()}
          >
            {handoffBusy ? (
              <span className="btn-busy">
                <Spinner />
                {stopOld ? "Starting and stopping…" : "Starting…"}
              </span>
            ) : (
              `Start ${agents.find((a) => a.id === handoff.agentId)?.name ?? "agent"}`
            )}
          </button>
        </>
      }
    >
      <div className="muted" style={{ marginBottom: 12, lineHeight: 1.6 }}>
        No agent CLI can resume another's session, so this carries the work rather
        than the conversation: the ticket, what has changed, and the tail of{" "}
        {handoff.from.title}'s terminal.
      </div>

      <Field label="Continue with">
        <select
          value={handoff.agentId}
          onChange={(e) => setHandoff({ ...handoff, agentId: e.target.value })}
        >
          {installed.map((a) => (
            <option key={a.id} value={a.id}>
              {a.name}{a.id === handoff.from.agent_id ? " (same agent)" : ""}
            </option>
          ))}
        </select>
      </Field>

      <Field
        label="Handoff briefing"
        hint={handoffLoading ? "Gathering the diff and terminal…" : "Edit freely before sending."}
      >
        <textarea
          rows={14}
          value={handoffPrompt}
          onChange={(e) => setHandoffPrompt(e.target.value)}
        />
      </Field>

      <label className="row" style={{ gap: 7, cursor: "pointer" }}>
        <input
          type="checkbox"
          style={{ width: "auto" }}
          checked={stopOld}
          onChange={(e) => setStopOld(e.target.checked)}
        />
        Stop {handoff.from.title} once the new agent starts
      </label>
    </Modal>
  );

  return { startHandoff, dialog };
}
