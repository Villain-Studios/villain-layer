import { useEffect, useMemo, useState } from "react";
import { api } from "../lib/api";
import { useStore } from "../store";
import type { TaskView } from "../lib/types";

/**
 * Where a prompt about a task should go: an agent already running in it, or
 * a new one started with the prompt as its opening message.
 */
export function useAgentTarget(task: TaskView) {
  const allPanes = useStore((s) => s.panes);
  const agents = useStore((s) => s.agents);
  const refreshPanes = useStore((s) => s.refreshPanes);
  const setTab = useStore((s) => s.setTab);

  const running = useMemo(
    () => allPanes.filter((p) => p.task_id === task.id && p.kind === "agent" && p.running),
    [allPanes, task.id],
  );
  const installed = useMemo(() => agents.filter((a) => a.installed), [agents]);
  const [target, setTarget] = useState("");
  const [agentId, setAgentId] = useState("");
  const [scope, setScope] = useState<string | null>(null);

  // The first agent still running is most likely the one that did the work.
  // A pick that has since exited falls back to it.
  useEffect(() => {
    if (!running.some((p) => p.id === target)) setTarget(running[0]?.id ?? "");
  }, [running, target]);
  useEffect(() => {
    if (!installed.some((a) => a.id === agentId)) setAgentId(installed[0]?.id ?? "");
  }, [installed, agentId]);

  const pane = running.find((p) => p.id === target) ?? null;

  return {
    running, installed, target, setTarget, agentId, setAgentId, scope, setScope, pane,
    /** Nothing is running and nothing could be started. */
    stuck: running.length === 0 && !agentId,
    /**
     * Send through `deliver`, which types into the pane when given one and
     * otherwise returns the prompt to start an agent with. Says who got it.
     */
    async send(deliver: (paneId: string | null, scope: string | null) => Promise<string>): Promise<string> {
      if (pane) {
        await deliver(pane.id, null);
        return pane.title;
      }
      const prompt = await deliver(null, scope);
      const started = await api.spawnAgent(task.id, agentId, scope, prompt);
      await refreshPanes();
      setTab("terminals");
      return started.title;
    },
  };
}

/** The selects for `useAgentTarget`, sized for a modal footer. */
export function AgentTargetFields({
  task, at, disabled,
}: {
  task: TaskView;
  at: ReturnType<typeof useAgentTarget>;
  disabled?: boolean;
}) {
  if (at.running.length === 0) {
    return (
      <>
        <label className="review-target">
          <span>Start</span>
          <select value={at.agentId} onChange={(e) => at.setAgentId(e.target.value)} disabled={disabled}>
            {at.installed.map((a) => <option key={a.id} value={a.id}>{a.name}</option>)}
          </select>
        </label>
        <label className="review-target">
          <span>in</span>
          <select
            value={at.scope ?? ""}
            onChange={(e) => at.setScope(e.target.value || null)}
            disabled={disabled}
          >
            <option value="">the task folder</option>
            {task.checkouts.map((c) => <option key={c.id} value={c.id}>only {c.project_name}</option>)}
          </select>
        </label>
      </>
    );
  }
  if (at.running.length === 1) return null;
  return (
    <label className="review-target">
      <span>Send to</span>
      <select value={at.target} onChange={(e) => at.setTarget(e.target.value)} disabled={disabled}>
        {at.running.map((p) => <option key={p.id} value={p.id}>{p.title}</option>)}
      </select>
    </label>
  );
}
