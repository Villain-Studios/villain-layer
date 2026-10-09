import { describe, expect, test } from "bun:test";
import {
  filterRuns,
  formatDuration,
  formatTokens,
  resultOf,
  runChoices,
  runDays,
  runStats,
  wentWrong,
} from "../src/lib/runs";
import type { Run } from "../src/lib/types-runs";

const now = new Date(2026, 9, 9, 15, 0).getTime();
const daysAgo = (d: number, minutes = 0) => new Date(now - d * 86_400_000 - minutes * 60_000).toISOString();

function run(over: Partial<Run> = {}): Run {
  return {
    id: "p",
    agent: "claude",
    task_id: "t1",
    task: "Fix the login",
    repos: [{ id: "api", name: "ACME API" }],
    acp: false,
    started_at: daysAgo(0, 10),
    ended_at: daysAgo(0),
    end: "exited",
    code: 0,
    loop: null,
    tokens: null,
    ...over,
  };
}

describe("what a run came to", () => {
  test("a loop's verdict outranks you stopping the agent once it was done", () => {
    expect(resultOf(run({ end: "stopped", loop: { used: 1, rounds: 5, end: "passed" } }))).toBe("passed");
    expect(resultOf(run({ end: "stopped", loop: { used: 5, rounds: 5, end: "gave_up" } }))).toBe("gave_up");
    expect(resultOf(run({ end: "stopped", loop: { used: 2, rounds: 5, end: "stopped" } }))).toBe("stopped");
  });

  test("an agent that failed failed, whatever its loop said", () => {
    expect(resultOf(run({ end: "failed", code: 3, loop: { used: 1, rounds: 5, end: "passed" } }))).toBe("failed");
  });

  test("only failing and giving up count as going wrong", () => {
    expect(wentWrong(run({ end: "failed" }))).toBe(true);
    expect(wentWrong(run({ end: "stopped", loop: { used: 5, rounds: 5, end: "gave_up" } }))).toBe(true);
    expect(wentWrong(run({ end: "stopped" }))).toBe(false);
    expect(wentWrong(run({ end: "exited" }))).toBe(false);
  });
});

describe("the filters", () => {
  const runs = [
    run({ id: "a", repos: [{ id: "api", name: "ACME API" }, { id: "web", name: "ACME Web" }] }),
    run({ id: "b", agent: "opencode", repos: [{ id: "web", name: "ACME Web" }] }),
  ];

  test("a run at a task's root counts for each of its repositories", () => {
    expect(filterRuns(runs, { agent: null, repo: "web" }).map((r) => r.id)).toEqual(["a", "b"]);
    expect(filterRuns(runs, { agent: null, repo: "api" }).map((r) => r.id)).toEqual(["a"]);
    expect(filterRuns(runs, { agent: "opencode", repo: "api" })).toEqual([]);
  });

  test("offer what the log has seen, by the latest name", () => {
    const older = run({ repos: [{ id: "web", name: "web (old name)" }] });
    expect(runChoices([...runs, older])).toEqual({
      agents: ["claude", "opencode"],
      repos: [{ id: "api", name: "ACME API" }, { id: "web", name: "ACME Web" }],
    });
  });
});

describe("the sums", () => {
  test("say how many went wrong, how long runs took, and the loops' rounds", () => {
    const s = runStats([
      run({ started_at: daysAgo(0, 10), ended_at: daysAgo(0) }),
      run({ end: "failed", code: 1, started_at: daysAgo(0, 30), ended_at: daysAgo(0) }),
      run({ end: "stopped", loop: { used: 3, rounds: 5, end: "passed" }, started_at: daysAgo(0, 20), ended_at: daysAgo(0) }),
      run({ end: "stopped", loop: { used: 5, rounds: 5, end: "gave_up" }, started_at: daysAgo(0, 20), ended_at: daysAgo(0) }),
    ]);
    expect([s.runs, s.wrong, s.wrongRate, s.looped, s.rounds]).toEqual([4, 2, 0.5, 2, 8]);
    expect(s.avgMs).toBe(20 * 60_000);
  });

  test("add up tokens only over the runs that said, and say how many did", () => {
    const tokens = { input: 10, output: 20, cached_read: 300, cached_write: 4 };
    const s = runStats([run({ tokens }), run({ tokens }), run()]);
    expect(s.tokens).toEqual({ input: 20, output: 40, cached_read: 600, cached_write: 8 });
    expect(s.withTokens).toBe(2);
    expect(runStats([run()]).tokens).toBeNull();
  });

  test("of nothing are nothing, not NaN", () => {
    expect(runStats([])).toMatchObject({ runs: 0, wrongRate: 0, avgMs: 0, tokens: null });
  });
});

describe("the chart", () => {
  test("has a bar for each day up to today, empty ones too, and leaves older runs out", () => {
    const days = runDays(
      [
        run({ ended_at: daysAgo(0) }),
        run({ ended_at: daysAgo(0), end: "failed" }),
        run({ ended_at: daysAgo(2), loop: { used: 5, rounds: 5, end: "gave_up" } }),
        run({ ended_at: daysAgo(20) }),
      ],
      14,
      now,
    );
    expect(days).toHaveLength(14);
    expect(days[13]).toEqual({ day: "2026-10-09", runs: 2, wrong: 1 });
    expect(days[12]).toEqual({ day: "2026-10-08", runs: 0, wrong: 0 });
    expect(days[11]).toEqual({ day: "2026-10-07", runs: 1, wrong: 1 });
    expect(days.reduce((n, d) => n + d.runs, 0)).toBe(3);
  });
});

describe("the numbers", () => {
  test("read short", () => {
    expect([formatDuration(45_000), formatDuration(192_000), formatDuration(3_900_000)]).toEqual(["45s", "3m 12s", "1h 5m"]);
    expect([formatTokens(950), formatTokens(12_345), formatTokens(4_500_000)]).toEqual(["950", "12.3k", "4.5M"]);
  });
});
