// The Markdown parser behind comment bodies (src/lib/markdown.ts).
// `bun test scripts`, part of `bun run check`. Outside src/ so tsc, which
// has no bun types, does not read it.
import { describe, expect, test } from "bun:test";
import { inline, parse } from "../src/lib/markdown";

describe("a comment as GitHub shows it", () => {
  test("a deployment bot's comment has a heading, bold, code and a link", () => {
    const tree = parse(
      "### 🚀 Preview deployed\n\n**Build:** `a1b2c3d`\n\n**Available at:**\n- https://acme-123.preview.example.com",
    );
    expect(tree[0]).toEqual({ t: "h", level: 3, c: [{ t: "text", v: "🚀 Preview deployed" }] });
    expect(tree[1]).toEqual({
      t: "p",
      c: [
        { t: "strong", c: [{ t: "text", v: "Build:" }] },
        { t: "text", v: " " },
        { t: "code", v: "a1b2c3d" },
      ],
    });
    expect(tree[3]).toEqual({
      t: "list",
      ordered: false,
      start: 1,
      items: [{
        task: null,
        c: [{ t: "p", c: [{ t: "link", href: "https://acme-123.preview.example.com", c: [{ t: "text", v: "https://acme-123.preview.example.com" }] }] }],
      }],
    });
  });

  test("a single newline breaks the line, as in GitHub comments", () => {
    expect(parse("one\ntwo")).toEqual([{ t: "p", c: [{ t: "text", v: "one" }, { t: "br" }, { t: "text", v: "two" }] }]);
  });

  test("code is left as written", () => {
    expect(parse("```ts\nconst a = b * c * d;\n```")).toEqual([{ t: "pre", lang: "ts", v: "const a = b * c * d;" }]);
    expect(inline("`a*b*c` and snake_case_name")).toEqual([
      { t: "code", v: "a*b*c" },
      { t: "text", v: " and snake_case_name" },
    ]);
  });

  test("lists nest, number and tick", () => {
    const [list] = parse("1. first\n   - inner\n2. [x] done");
    expect(list.t === "list" && list.ordered).toBe(true);
    if (list.t !== "list") return;
    expect(list.items[0].c[1]).toMatchObject({ t: "list", ordered: false });
    expect(list.items[1].task).toBe(true);
  });

  test("a table keeps its columns and alignment", () => {
    const [table] = parse("| File | Coverage |\n|:--|--:|\n| a.ts | 91% |\n| b.ts | 40% |");
    expect(table).toMatchObject({ t: "table", align: ["left", "right"] });
    if (table.t === "table") expect(table.rows).toHaveLength(2);
  });
});

describe("what a comment cannot do", () => {
  test("HTML is never markup: tags go, their text stays, markers vanish", () => {
    const tree = parse("<!-- Sticky Pull Request Comment -->\n<details>\n<summary>Coverage</summary>\n\n<script>alert(1)</script> fine\n</details>");
    const text = JSON.stringify(tree);
    expect(text).not.toContain("Sticky");
    expect(text).not.toContain("<details>");
    expect(text).toContain("Coverage");
    // Not a tag the renderer knows: shown as the text it is, never run.
    expect(text).toContain("<script>alert(1)</script> fine");
  });

  test("a javascript: link is text, and images are not loaded", () => {
    expect(inline("[click](javascript:alert(1))")).toEqual([{ t: "text", v: "click" }]);
    expect(inline("![logo](https://tracker.example/pixel.png)")).toEqual([{ t: "text", v: "[image: logo]" }]);
  });
});
