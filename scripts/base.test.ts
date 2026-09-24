import { describe, expect, test } from "bun:test";
import { suggestBase } from "../src/lib/derive";

/** Pick repos one change at a time, the way the dialogs do. */
function pick(steps: string[][], typed?: { at: number; base: string }) {
  let box = "";
  let last = "";
  steps.forEach((defaults, i) => {
    box = defaults.length === 0 ? "" : suggestBase(box, last, defaults);
    last = defaults.length === 0 ? last : suggestBase("", "", defaults);
    if (typed?.at === i) box = typed.base;
  });
  return box;
}

describe("the branch a new task starts from", () => {
  test("is the picked repos' default when they share one", () => {
    expect(suggestBase("", "", ["development", "development"])).toBe("development");
  });

  test("is left blank, for each repo's own default, when they differ", () => {
    expect(suggestBase("", "", ["dev", "development"])).toBe("");
  });

  test("follows the repos when one suggestion is swapped for another", () => {
    // customer-portal alone, then with analytics-service, then analytics-service
    // and flight-service-v2 without it: the box stayed on `dev`.
    expect(pick([["dev"], ["dev", "development"], ["development"], ["development", "development"]]))
      .toBe("development");
  });

  test("keeps a base typed by hand when another repo is picked", () => {
    expect(pick([["main"], ["main", "main"]], { at: 0, base: "release/2.1" })).toBe("release/2.1");
  });
});
