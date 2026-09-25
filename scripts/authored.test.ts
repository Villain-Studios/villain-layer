import { describe, expect, test } from "bun:test";
import { authoredStanding, taskOfPr } from "../src/lib/derive";
import type { AuthoredPr, CheckoutPr } from "../src/lib/types";

function pr(extra: Partial<AuthoredPr> = {}): AuthoredPr {
  return {
    repo: "acme/api", number: 7, title: "t", url: "https://github.com/acme/api/pull/7", draft: false,
    updated_at: "", checks: "passing", review: "approved", approved_by: ["ana"], changes_by: [],
    waiting_on: [], unresolved: 0, unresolved_more: false, conflicts: false, ...extra,
  };
}

describe("where a pull request of yours stands (REV-4)", () => {
  test("approved and green is ready to merge", () => {
    expect(authoredStanding(pr()).label).toBe("ready to merge");
    // No review required, nobody asked: nothing to wait for either.
    expect(authoredStanding(pr({ review: "none", approved_by: [], checks: "none" })).tone).toBe("good");
  });
  test("anything you have to fix outranks anything you wait for", () => {
    for (const bad of [
      { conflicts: true }, { checks: "failing" as const }, { unresolved: 2 },
      { review: "changes_requested" as const, checks: "pending" as const },
    ]) {
      expect(authoredStanding(pr(bad)).label).toBe("needs you");
    }
  });
  test("waits on checks, then on reviewers", () => {
    expect(authoredStanding(pr({ checks: "pending", review: "review_required" })).label).toBe("checks running");
    expect(authoredStanding(pr({ review: "review_required", approved_by: [] })).label).toBe("waiting on review");
    expect(authoredStanding(pr({ review: "none", approved_by: [], waiting_on: ["@fe"] })).label).toBe("waiting on review");
  });
  test("a draft is a draft, whatever else is true of it", () => {
    expect(authoredStanding(pr({ draft: true, checks: "failing" })).tone).toBe("draft");
  });
});

describe("the task a pull request belongs to", () => {
  const row = (url: string, past: string[] = []) =>
    ({ pr: { url }, past: past.map((u) => ({ url: u })) }) as unknown as CheckoutPr;
  const prs = { "t-1": [row("https://x/pull/1")], "t-2": [row("https://x/pull/3", ["https://x/pull/2"])] };
  test("is found by its URL, open or earlier", () => {
    expect(taskOfPr("https://x/pull/1", prs)).toBe("t-1");
    expect(taskOfPr("https://x/pull/2", prs)).toBe("t-2");
  });
  test("is none for one no task made", () => {
    expect(taskOfPr("https://x/pull/9", prs)).toBeNull();
  });
});
