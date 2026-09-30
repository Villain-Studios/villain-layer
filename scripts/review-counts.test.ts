import { describe, expect, test } from "bun:test";
import { reviewCounts } from "../src/lib/derive";
import type { AuthoredPr, ReviewQueue, ReviewRequest } from "../src/lib/types";

const req = (number: number) => ({ number }) as ReviewRequest;
const mine = (number: number) => ({ number }) as AuthoredPr;

function queue(extra: Partial<ReviewQueue> = {}): ReviewQueue {
  return {
    mine: [req(1)],
    mine_more: false,
    team: null,
    authored: { prs: [1, 2, 3, 4, 5, 6, 7].map(mine), more: false, error: null },
    ...extra,
  };
}

describe("what the Reviews view counts", () => {
  test("counts both sides, not their sum", () => {
    expect(reviewCounts(queue())).toEqual({ toReview: "1", yours: "7" });
  });

  test("leaves out a team whose lookup failed", () => {
    const team = { slug: "acme/fe", name: "fe", prs: [req(2)], more: false, error: "no read:org" };
    expect(reviewCounts(queue({ team })).toReview).toBe("1");
  });

  test("says when a list was cut", () => {
    expect(reviewCounts(queue({ mine_more: true })).toReview).toBe("1+");
  });

  test("has no count for your PRs when they could not be read", () => {
    expect(reviewCounts(queue({ authored: { prs: [], more: false, error: "rate limited" } })).yours).toBeUndefined();
  });

  test("has nothing before GitHub answers", () => {
    expect(reviewCounts(null)).toEqual({});
  });
});
