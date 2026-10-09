import { describe, expect, test } from "bun:test";
import { approveWarning, questions, sameAnswer, setAnswer } from "../src/lib/questions";

const DRAFT = `## Requirements
- R-1: WHEN a sixth reset is asked within an hour THE SYSTEM SHALL refuse it.

## Out of scope
- Rate-limiting sign-ins.

## Open questions
- Q-1: Should support staff be able to lift the limit?
  - Yes, from the admin page
  - No
- Q-2: Should the refusal say when to try again?
  - Yes, the minutes left
  - No
`;

describe("open questions in the requirements (SPEC-1, SPEC-19)", () => {
  test("each question is read with its id and the answers drafted for it", () => {
    const qs = questions(DRAFT);
    expect(qs.map((q) => q.id)).toEqual(["Q-1", "Q-2"]);
    expect(qs[0].text).toBe("Should support staff be able to lift the limit?");
    expect(qs[0].options).toEqual(["Yes, from the admin page", "No"]);
    expect(qs.every((q) => q.answer === null)).toBe(true);
  });

  test("an answer typed beside a question with dashes counts as its answer", () => {
    const typed = `## Open questions
- What should a refusal say: "try later" or the minutes left?
-- Answer: the minutes left
- Should the API ship first?
-- yes
- Which mail provider?
`;
    const qs = questions(typed);
    expect(qs.map((q) => [q.id, q.answer])).toEqual([["Q-1", "the minutes left"], ["Q-2", "yes"], ["Q-3", null]]);
  });

  test("none, a heading of another name, or a code block hold no questions", () => {
    expect(questions("## Open questions\nNone.\n")).toEqual([]);
    expect(questions("## Open questions\n- None.\n")).toEqual([]);
    expect(questions("## Out of scope\n- Q-1: not a question\n")).toEqual([]);
    expect(questions("## Open questions\n```\n- Q-1: in code\n```\n")).toEqual([]);
  });

  test("a question wrapped onto a second line is one question", () => {
    const qs = questions("## Open questions\n- Q-1: Should staff be able\n  to lift the limit?\n  - Yes\n");
    expect(qs[0].text).toBe("Should staff be able to lift the limit?");
    expect(qs[0].options).toEqual(["Yes"]);
  });

  test("answering writes an Answer line under the question, and answering again replaces it", () => {
    const once = setAnswer(DRAFT, "Q-1", "No");
    expect(once).toContain("  - No\n  Answer: No\n- Q-2:");
    expect(questions(once)[0].answer).toBe("No");
    const twice = setAnswer(once, "Q-1", "Yes, from the admin page");
    expect(twice.match(/Answer:/g)?.length).toBe(1);
    expect(questions(twice)[0].answer).toBe("Yes, from the admin page");
    // The last question, and the text after it, keep their place.
    const last = setAnswer(DRAFT, "Q-2", "Yes, the minutes left");
    expect(last.endsWith("  - No\n  Answer: Yes, the minutes left\n")).toBe(true);
  });

  test("an empty answer takes the answer away, a dashed one included", () => {
    const typed = "## Open questions\n- Q-1: Ship first?\n-- yes\n\n## Notes\nx\n";
    const cleared = setAnswer(typed, "Q-1", "  ");
    expect(cleared).toBe("## Open questions\n- Q-1: Ship first?\n\n## Notes\nx\n");
    expect(questions(cleared)[0].answer).toBeNull();
  });

  test("a picked answer matches its option whatever the spacing or case", () => {
    expect(sameAnswer("Yes, the minutes left", "  yes,  the minutes left ")).toBe(true);
    expect(sameAnswer("No", null)).toBe(false);
  });
});

describe("approving with questions still there (SPEC-21)", () => {
  test("says how many have no answer and how many are not built in", () => {
    expect(approveWarning([])).toBeNull();
    const one = approveWarning(questions(DRAFT.replace("  - No\n- Q-2", "  - No\n  Answer: No\n- Q-2")));
    expect(one).toContain("1 open question has no answer");
    expect(one).toContain("1 answer is not built into the requirements yet");
    const none = approveWarning(questions(DRAFT));
    expect(none).toContain("2 open questions have no answer");
    expect(none).not.toContain("not built into");
  });
});
