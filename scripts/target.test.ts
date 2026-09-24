import { describe, expect, test } from "bun:test";
import { route } from "../src/lib/target";

const known = {
  tasks: ["t1", "t2"],
  panes: [
    { id: "p1", task_id: "t1" },
    { id: "c1", task_id: "chat" },
  ],
};

describe("a click on a banner, a toast or a message", () => {
  test("a pane lands on its task with that pane selected", () => {
    expect(route("pane:t1:p1", known)).toEqual({ view: "work", task: "t1", pane: "p1" });
  });

  test("a pane from before a restart lands on its task", () => {
    expect(route("pane:t1:gone", known)).toEqual({ view: "work", task: "t1" });
  });

  test("a chat pane lands on Chat with that chat", () => {
    expect(route("pane:chat:c1", known)).toEqual({ view: "chat", pane: "c1" });
    expect(route("pane:chat:gone", known)).toEqual({ view: "chat" });
  });

  test("a deleted task lands on the Work overview", () => {
    expect(route("pane:t9:p1", known)).toEqual({ view: "work", task: null });
    expect(route("task:t9", known)).toEqual({ view: "work", task: null });
    expect(route("pr:t9", known)).toEqual({ view: "work", task: null });
  });

  test("pull request news opens the task's Pull requests tab", () => {
    expect(route("pr:t2", known)).toEqual({ view: "work", task: "t2", tab: "pr" });
  });

  test("a ticket and a review request focus what they name", () => {
    expect(route("ticket:ACME-3", known)).toEqual({ view: "tickets", issue: "ACME-3" });
    expect(route("review:o/a#7", known)).toEqual({ view: "reviews", review: "o/a#7" });
  });

  test("the targets banners have always sent still work", () => {
    expect(route("reviews", known)).toEqual({ view: "reviews" });
    expect(route("tickets", known)).toEqual({ view: "tickets" });
    expect(route("chat", known)).toEqual({ view: "chat" });
    expect(route("work", known)).toEqual({ view: "work", task: null });
    expect(route("task:t1", known)).toEqual({ view: "work", task: "t1" });
  });

  test("an unknown target goes nowhere", () => {
    expect(route("elsewhere", known)).toBeNull();
    expect(route("pane:", known)).toBeNull();
    expect(route("pane:nocolon", known)).toBeNull();
    expect(route("ticket:", known)).toBeNull();
  });
});
