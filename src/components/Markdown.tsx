import { useMemo, type MouseEvent, type ReactNode } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { parse, safeHref, type Block, type Inline } from "../lib/markdown";

/**
 * Markdown as GitHub shows it, built from React elements only (see
 * `lib/markdown.ts` for why never HTML). Links open in the browser, and a
 * click on one does not reach whatever row the text sits in.
 */
export function Markdown({ text }: { text: string }) {
  const tree = useMemo(() => parse(text), [text]);
  return <div className="md">{tree.map(block)}</div>;
}

function open(e: MouseEvent, href: string) {
  e.preventDefault();
  e.stopPropagation();
  if (safeHref(href)) void openUrl(href).catch(() => {});
}

function inlines(nodes: Inline[]): ReactNode[] {
  return nodes.map((n, i) => {
    switch (n.t) {
      case "text": return n.v;
      case "code": return <code key={i}>{n.v}</code>;
      case "strong": return <strong key={i}>{inlines(n.c)}</strong>;
      case "em": return <em key={i}>{inlines(n.c)}</em>;
      case "del": return <del key={i}>{inlines(n.c)}</del>;
      case "br": return <br key={i} />;
      case "link":
        return (
          <a key={i} href={n.href} title={n.href} onClick={(e) => open(e, n.href)}>
            {inlines(n.c)}
          </a>
        );
    }
  });
}

function block(b: Block, i: number): ReactNode {
  switch (b.t) {
    case "h": {
      const H = `h${b.level}` as "h1";
      return <H key={i}>{inlines(b.c)}</H>;
    }
    case "p": return <p key={i}>{inlines(b.c)}</p>;
    case "pre": return <pre key={i}><code>{b.v}</code></pre>;
    case "quote": return <blockquote key={i}>{b.c.map(block)}</blockquote>;
    case "hr": return <hr key={i} />;
    case "table":
      return (
        <div key={i} className="md-table">
          <table>
            <thead>
              <tr>{b.head.map((c, n) => <th key={n} style={{ textAlign: b.align[n] ?? undefined }}>{inlines(c)}</th>)}</tr>
            </thead>
            <tbody>
              {b.rows.map((row, r) => (
                <tr key={r}>
                  {row.map((c, n) => <td key={n} style={{ textAlign: b.align[n] ?? undefined }}>{inlines(c)}</td>)}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
    case "list": {
      const items = b.items.map((item, n) => {
        // A one-paragraph item sits on its bullet's line, as GitHub's do.
        const [head, ...rest] = item.c;
        const lead = head?.t === "p" ? inlines(head.c) : head ? block(head, 0) : null;
        return (
          <li key={n} className={item.task === null ? undefined : "md-task"}>
            {item.task !== null && <span className="md-box">{item.task ? "☑" : "☐"}</span>}
            {lead}
            {rest.map(block)}
          </li>
        );
      });
      return b.ordered
        ? <ol key={i} start={b.start}>{items}</ol>
        : <ul key={i}>{items}</ul>;
    }
  }
}
