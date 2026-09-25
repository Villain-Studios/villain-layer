import { describe, expect, test } from "bun:test";
import { paneName } from "../src/lib/derive";
import type { PaneInfo } from "../src/lib/types";

function pane(title: string, topic: string | null): PaneInfo {
  return {
    id: "p", task_id: "t", checkout_id: null, kind: "agent", title, agent_id: "claude",
    cwd: "/tmp", running: true, exit_code: null, started_at: "", last_output_at: "",
    notice: null, activity: "idle", activity_since: "", topic,
  };
}

describe("what a pane is called (PANE-12)", () => {
  test("is the agent's name until the conversation has one", () => {
    expect(paneName(pane("Claude Code · api", null))).toBe("Claude Code · api");
  });
  test("is the conversation's name in place of the agent's, keeping the scope", () => {
    expect(paneName(pane("Claude Code · api", "Fix the login"))).toBe("Fix the login · api");
    expect(paneName(pane("Claude Code · api (resumed)", "Fix the login"))).toBe("Fix the login · api");
  });
  test("is only the conversation's name in a chat, which has no scope", () => {
    expect(paneName(pane("Claude Code (resumed)", "Release blockers"))).toBe("Release blockers");
  });
});
