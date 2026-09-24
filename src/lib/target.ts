/**
 * Where a target — from a banner, a toast or a message — lands (NOTE-4).
 *
 * Pure, so it can be tested without a store or a window: `goto.ts` applies
 * what this decides. What a target names may have gone since it was made —
 * every restart gives panes new ids, a task can be deleted — and each gone
 * thing falls back to the nearest place that still exists.
 */
import type { Target } from "./types";

export type Step =
  | { view: "work"; task: string | null; tab?: "pr"; pane?: string }
  | { view: "chat"; pane?: string }
  | { view: "reviews"; review?: string }
  | { view: "tickets"; issue?: string };

export interface Known {
  tasks: string[];
  panes: { id: string; task_id: string }[];
}

/** The id of the Chat view's pseudo-task, as `CHAT_TASK_ID` in `derive.ts`. */
const CHAT = "chat";

function after(target: string, prefix: string): string | null {
  return target.startsWith(prefix) && target.length > prefix.length
    ? target.slice(prefix.length)
    : null;
}

export function route(target: Target | string, known: Known): Step | null {
  const taskStep = (id: string, extra: { tab?: "pr"; pane?: string } = {}): Step =>
    id === CHAT
      ? { view: "chat", ...(extra.pane ? { pane: extra.pane } : {}) }
      : known.tasks.includes(id)
        ? { view: "work", task: id, ...extra }
        // Deleted since: the overview, not a blank task.
        : { view: "work", task: null };

  if (target === "reviews" || target === "tickets") return { view: target };
  if (target === "chat") return { view: "chat" };
  if (target === "work") return { view: "work", task: null };

  const task = after(target, "task:");
  if (task) return taskStep(task);

  const pane = after(target, "pane:");
  if (pane) {
    // Pane ids never hold a colon; a task id is whatever came before.
    const at = pane.lastIndexOf(":");
    if (at <= 0) return null;
    const taskId = pane.slice(0, at);
    const paneId = pane.slice(at + 1);
    const alive = known.panes.some((p) => p.id === paneId && p.task_id === taskId);
    return taskStep(taskId, alive ? { pane: paneId } : {});
  }

  const pr = after(target, "pr:");
  if (pr) return known.tasks.includes(pr) ? { view: "work", task: pr, tab: "pr" } : taskStep(pr);

  const issue = after(target, "ticket:");
  if (issue) return { view: "tickets", issue };

  const review = after(target, "review:");
  if (review) return { view: "reviews", review };

  return null;
}
