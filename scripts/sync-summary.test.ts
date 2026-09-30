import { describe, expect, test } from "bun:test";
import { syncSummary } from "../src/lib/report";
import type { Synced } from "../src/lib/types";

const row = (repo: string, ok: boolean): Synced => ({ project_id: `p-${repo}`, repo, ok, detail: ok ? "Fetched." : "git: failed" });

describe("the line a Sync of several repos ends with", () => {
  test("names the repo that failed", () => {
    expect(syncSummary([row("api", true), row("shared-actions", false), row("web", true)])).toBe(
      "Synced 2 of 3. Failed: shared-actions. Each says why on its row.",
    );
  });

  test("names five and counts the rest", () => {
    const rows = ["a", "b", "c", "d", "e", "f", "g"].map((r) => row(r, false));
    expect(syncSummary([row("ok", true), ...rows])).toBe(
      "Synced 1 of 8. Failed: a, b, c, d, e and 2 more. Each says why on its row.",
    );
  });

  test("says only how many when none failed", () => {
    expect(syncSummary([row("api", true), row("web", true)])).toBe("Synced 2 repositories.");
  });
});
