/**
 * GitHub-flavoured Markdown, parsed into a tree the UI renders as React
 * elements (`components/Markdown.tsx`). Never HTML: comment bodies are
 * written by other people and bots, and markup from them in the webview
 * could reach the app's own commands. Raw HTML tags are dropped, keeping
 * their text; `<br>` is a line break, and `<!-- … -->` markers bots leave
 * are removed.
 *
 * Covers what review comments and bots use: headings, emphasis, code,
 * fenced blocks, lists (nested, numbered, task), quotes, tables, links,
 * bare URLs and rules. As GitHub does in comments, a single newline breaks
 * the line. Tested by `scripts/markdown.test.ts`.
 */

export type Inline =
  | { t: "text"; v: string }
  | { t: "code"; v: string }
  | { t: "strong" | "em" | "del"; c: Inline[] }
  | { t: "link"; href: string; c: Inline[] }
  | { t: "br" };

export type Align = "left" | "center" | "right" | null;

export type Block =
  | { t: "h"; level: number; c: Inline[] }
  | { t: "p"; c: Inline[] }
  | { t: "pre"; lang: string; v: string }
  | { t: "quote"; c: Block[] }
  | { t: "list"; ordered: boolean; start: number; items: { task: boolean | null; c: Block[] }[] }
  | { t: "table"; align: Align[]; head: Inline[][]; rows: Inline[][][] }
  | { t: "hr" };

export function parse(src: string): Block[] {
  return blocks(src.replace(/\r\n?/g, "\n").replace(/<!--[\s\S]*?-->/g, "").split("\n"));
}

// ------------------------------------------------------------------ blocks

const FENCE = /^\s{0,3}(`{3,}|~{3,})\s*([\w+#.-]*)/;
const HEADING = /^\s{0,3}(#{1,6})\s+(.*?)(?:\s+#+)?\s*$/;
const RULE = /^\s{0,3}([-*_])(?:\s*\1){2,}\s*$/;
const QUOTE = /^\s{0,3}>/;
const ITEM = /^(\s*)([-*+]|\d{1,9}[.)])\s+(.*)$/;
const TABLE_SEP = /^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$/;

/** Tags GitHub renders that mean nothing here but their text. */
const TAG =
  /<\/?(?:details|summary|p|div|span|sub|sup|b|strong|i|em|img|a|picture|source|kbd|table|thead|tbody|tr|td|th|h[1-6]|ul|ol|li|hr|code|pre|center|font|dl|dt|dd|ins|del|s|u|small|big)\b[^>]*>/gi;

/** A line of nothing but such tags (`<details>`) is a blank line. */
const blank = (line: string) => line.replace(TAG, "").trim() === "";

const indentOf = (line: string) => line.length - line.trimStart().length;

function isTable(lines: string[], i: number): boolean {
  return lines[i].includes("|") && i + 1 < lines.length && lines[i + 1].includes("-") && TABLE_SEP.test(lines[i + 1]);
}

function startsBlock(lines: string[], i: number): boolean {
  const l = lines[i];
  return FENCE.test(l) || HEADING.test(l) || RULE.test(l) || QUOTE.test(l) || ITEM.test(l) || isTable(lines, i);
}

function blocks(lines: string[]): Block[] {
  const out: Block[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (blank(line)) {
      i++;
      continue;
    }
    const fence = FENCE.exec(line);
    if (fence) {
      const body: string[] = [];
      i++;
      while (i < lines.length && !lines[i].trimStart().startsWith(fence[1])) body.push(lines[i++]);
      i++;
      out.push({ t: "pre", lang: fence[2], v: body.join("\n") });
      continue;
    }
    const heading = HEADING.exec(line);
    if (heading) {
      out.push({ t: "h", level: heading[1].length, c: inline(heading[2]) });
      i++;
      continue;
    }
    if (RULE.test(line)) {
      out.push({ t: "hr" });
      i++;
      continue;
    }
    if (QUOTE.test(line)) {
      const inner: string[] = [];
      while (i < lines.length && QUOTE.test(lines[i])) inner.push(lines[i++].replace(/^\s{0,3}>\s?/, ""));
      out.push({ t: "quote", c: blocks(inner) });
      continue;
    }
    if (isTable(lines, i)) {
      const head = cells(line);
      const align = cells(lines[i + 1]).map((c): Align =>
        c.startsWith(":") && c.endsWith(":") ? "center" : c.endsWith(":") ? "right" : c.startsWith(":") ? "left" : null,
      );
      i += 2;
      const rows: Inline[][][] = [];
      while (i < lines.length && !blank(lines[i]) && lines[i].includes("|")) {
        rows.push(cells(lines[i++]).map(inline));
      }
      out.push({ t: "table", align, head: head.map(inline), rows });
      continue;
    }
    if (ITEM.test(line)) {
      i = list(lines, i, out);
      continue;
    }
    // A paragraph runs to a blank line or the start of another block.
    const para = [line.trim()];
    i++;
    while (i < lines.length && !blank(lines[i]) && !startsBlock(lines, i)) para.push(lines[i++].trim());
    const c: Inline[] = [];
    para.forEach((l, n) => {
      if (n > 0) c.push({ t: "br" });
      c.push(...inline(l));
    });
    out.push({ t: "p", c });
  }
  return out;
}

/** A list starting at `lines[i]`, pushed onto `out`; returns where it ended. */
function list(lines: string[], i: number, out: Block[]): number {
  const first = ITEM.exec(lines[i])!;
  const indent = first[1].length;
  const ordered = /\d/.test(first[2]);
  const items: { task: boolean | null; c: Block[] }[] = [];
  while (i < lines.length) {
    const m = ITEM.exec(lines[i]);
    if (!m || m[1].length !== indent || /\d/.test(m[2]) !== ordered) break;
    const body = [m[3]];
    // Where the item's own text starts: nested lines are indented to it.
    const offset = m[1].length + m[2].length + 1;
    i++;
    while (i < lines.length) {
      const l = lines[i];
      if (blank(l)) {
        if (i + 1 < lines.length && !blank(lines[i + 1]) && indentOf(lines[i + 1]) > indent) {
          body.push("");
          i++;
          continue;
        }
        break;
      }
      if (indentOf(l) > indent) {
        body.push(l.slice(Math.min(indentOf(l), offset)));
        i++;
        continue;
      }
      if (startsBlock(lines, i)) break;
      body.push(l);
      i++;
    }
    let task: boolean | null = null;
    const box = /^\[([ xX])\]\s+/.exec(body[0]);
    if (box) {
      task = box[1] !== " ";
      body[0] = body[0].slice(box[0].length);
    }
    items.push({ task, c: blocks(body) });
  }
  out.push({ t: "list", ordered, start: ordered ? parseInt(first[2], 10) : 1, items });
  return i;
}

function cells(line: string): string[] {
  let s = line.trim();
  if (s.startsWith("|")) s = s.slice(1);
  if (s.endsWith("|") && !s.endsWith("\\|")) s = s.slice(0, -1);
  return s.split(/(?<!\\)\|/).map((c) => c.trim().replace(/\\\|/g, "|"));
}

// ------------------------------------------------------------------ inline

// A URL may hold one level of balanced parentheses, as Wikipedia's do.
const LINK = /^\[([^\]]*)\]\(\s*<?((?:[^()\s>]|\([^()\s]*\))*)>?(?:\s+"[^"]*")?\s*\)/;
const URL = /https?:\/\/[^\s<>()]*[^\s<>().,;:!?'"*_~]/g;
const ESCAPABLE = /[\\`*_{}[\]()#+\-.!|~<>]/;

/** Only these open anything; a `javascript:` link stays text. */
export const safeHref = (href: string) => /^(https?:|mailto:)/i.test(href);

const MARKS: [string, "strong" | "em" | "del"][] = [
  ["**", "strong"],
  ["__", "strong"],
  ["~~", "del"],
  ["*", "em"],
  ["_", "em"],
];

/** Where `mark`, opened at `from`, closes; -1 when it does not. */
function closing(s: string, from: number, mark: string): number {
  for (let j = from + 1; j <= s.length - mark.length; j++) {
    if (s[j] === "`") {
      // Code spans are opaque: a `*` inside one closes nothing.
      const run = /^`+/.exec(s.slice(j))![0];
      const end = s.indexOf(run, j + run.length);
      if (end !== -1) j = end + run.length - 1;
      continue;
    }
    if (!s.startsWith(mark, j) || /\s/.test(s[j - 1])) continue;
    // A single mark is not half of a double one.
    if (mark.length === 1 && (s[j + 1] === mark || s[j - 1] === mark)) continue;
    // `snake_case_name` is not emphasis.
    if (mark[0] === "_" && /\w/.test(s[j + mark.length] ?? "")) continue;
    return j;
  }
  return -1;
}

export function inline(s: string): Inline[] {
  const out: Inline[] = [];
  let buf = "";
  const flush = () => {
    if (!buf) return;
    const text = buf.replace(TAG, "");
    buf = "";
    let last = 0;
    for (const m of text.matchAll(URL)) {
      if (m.index! > last) out.push({ t: "text", v: text.slice(last, m.index) });
      out.push({ t: "link", href: m[0], c: [{ t: "text", v: m[0] }] });
      last = m.index! + m[0].length;
    }
    if (last < text.length) out.push({ t: "text", v: text.slice(last) });
  };

  let i = 0;
  outer: while (i < s.length) {
    const ch = s[i];
    const rest = s.slice(i);
    if (ch === "\\" && ESCAPABLE.test(s[i + 1] ?? "")) {
      buf += s[i + 1];
      i += 2;
      continue;
    }
    if (ch === "`") {
      const run = /^`+/.exec(rest)![0];
      const end = s.indexOf(run, i + run.length);
      if (end !== -1) {
        flush();
        out.push({ t: "code", v: s.slice(i + run.length, end).replace(/^ (.+) $/, "$1") });
        i = end + run.length;
      } else {
        buf += run;
        i += run.length;
      }
      continue;
    }
    if (ch === "!" && s[i + 1] === "[") {
      const m = LINK.exec(rest.slice(1));
      if (m) {
        // Not loaded: an image from a comment is a request to anywhere.
        flush();
        out.push({ t: "text", v: m[1] ? `[image: ${m[1]}]` : "[image]" });
        i += 1 + m[0].length;
        continue;
      }
    }
    if (ch === "[") {
      const m = LINK.exec(rest);
      if (m) {
        flush();
        const c = inline(m[1] || m[2]);
        out.push(...(safeHref(m[2]) ? [{ t: "link" as const, href: m[2], c }] : c));
        i += m[0].length;
        continue;
      }
    }
    if (ch === "<") {
      const auto = /^<((?:https?:|mailto:)[^\s>]+)>/i.exec(rest);
      if (auto) {
        flush();
        out.push({ t: "link", href: auto[1], c: [{ t: "text", v: auto[1] }] });
        i += auto[0].length;
        continue;
      }
      const br = /^<br\s*\/?>/i.exec(rest);
      if (br) {
        flush();
        out.push({ t: "br" });
        i += br[0].length;
        continue;
      }
    }
    for (const [mark, t] of MARKS) {
      if (!rest.startsWith(mark) || /\s/.test(s[i + mark.length] ?? " ")) continue;
      if (mark[0] === "_" && /\w/.test(s[i - 1] ?? "")) continue;
      const end = closing(s, i + mark.length - 1, mark);
      if (end === -1) continue;
      flush();
      out.push({ t, c: inline(s.slice(i + mark.length, end)) });
      i = end + mark.length;
      continue outer;
    }
    buf += ch;
    i++;
  }
  flush();
  return out;
}
