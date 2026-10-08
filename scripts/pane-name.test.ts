import { describe, expect, test } from "bun:test";
import { canRestart, paneName } from "../src/lib/derive";
import type { AgentStatus, PaneInfo } from "../src/lib/types";

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

describe("what can be restarted on its conversation (PANE-14)", () => {
  const agents: AgentStatus[] = [
    { id: "claude", name: "Claude Code", program: "claude", installed: true, path: "/bin/claude", resumes: true },
    { id: "gemini", name: "Gemini CLI", program: "gemini", installed: true, path: "/bin/gemini", resumes: false },
  ];
  test("an agent whose CLI resumes, running or exited", () => {
    expect(canRestart(pane("Claude Code", null), agents)).toBe(true);
    expect(canRestart({ ...pane("Claude Code", null), running: false, exit_code: 0 }, agents)).toBe(true);
  });
  test("not one that would lose its conversation, nor a shell", () => {
    expect(canRestart({ ...pane("Gemini CLI", null), agent_id: "gemini" }, agents)).toBe(false);
    expect(canRestart({ ...pane("Shell", null), kind: "shell", agent_id: null }, agents)).toBe(false);
  });
});
