import { describe, expect, test } from "bun:test";
import { readable, type Row } from "../src/phone/readable";

const rows = (...lines: (string | [string, true])[]): Row[] =>
  lines.map((l) => (typeof l === "string" ? { text: l, wrapped: false } : { text: l[0], wrapped: true }));

describe("a terminal's screen as text a phone can read (PHONE-6)", () => {
  test("puts back together a line the terminal wrapped at its width", () => {
    expect(readable(rows("Host tests pass now; that was ", ["the failure.", true]))).toEqual([
      "Host tests pass now; that was the failure.",
    ]);
  });

  test("drops a box's frame and keeps what is in it", () => {
    const box = rows(
      "╭──────────────────────╮",
      "│ Bash command         │",
      "│   npm test           │",
      "│ ❯ 1. Yes             │",
      "╰──────────────────────╯",
    );
    expect(readable(box)).toEqual(["Bash command", "  npm test", "❯ 1. Yes"]);
  });

  test("turns a rule into one gap, and leaves no blank screen below", () => {
    expect(readable(rows("done", "", "────────", "", "❯ ", "", "", ""))).toEqual(["done", "", "❯"]);
  });

  test("closes up padding that only lined things up at full width", () => {
    expect(readable(rows("auto mode on            (shift+tab to cycle)"))).toEqual([
      "auto mode on  (shift+tab to cycle)",
    ]);
    expect(readable(rows("    indented code stays indented"))).toEqual(["    indented code stays indented"]);
  });
});
