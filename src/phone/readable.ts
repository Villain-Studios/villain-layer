/**
 * A terminal's screen as text a phone can read (PHONE-6).
 *
 * Drawn as a terminal, an agent's 100-odd columns fitted to a phone came out
 * at six pixels a letter: there, but unreadable. The phone still runs the
 * output through a real terminal, so cursor moves and repaints land where
 * they should, and then reads its rows back as text, to be wrapped at a
 * normal size. What only makes sense at full width goes: the frames around
 * boxes, rules of dashes, and the padding that lined them up.
 */

/** One row of the terminal's buffer, as xterm gives it. */
export interface Row {
  text: string;
  /** This row is the overflow of the one before, which was too long for it. */
  wrapped: boolean;
}

/** Box-drawing and block characters only, with spaces between. */
const RULE = /^[\s─-╿▀-▟]+$/;
/** A box's left and right walls. */
const WALLS = /^[│┃║]\s?|\s?[│┃║]$/g;

export function readable(rows: Row[]): string[] {
  // Rows the terminal wrapped back into the lines the program wrote.
  const lines: string[] = [];
  for (const r of rows) {
    if (r.wrapped && lines.length) lines[lines.length - 1] += r.text;
    else lines.push(r.text);
  }

  const out: string[] = [];
  for (const raw of lines) {
    const line = raw.trimEnd();
    if (RULE.test(line)) {
      // A frame's top or bottom, or a rule: a gap, once.
      if (out.length && out[out.length - 1] !== "") out.push("");
      continue;
    }
    const inner = line.trimStart().startsWith("│") || line.trimStart().startsWith("┃") || line.trimStart().startsWith("║")
      ? line.trim().replace(WALLS, "").trimEnd()
      : line;
    // Runs of padding inside a line were alignment at full width; at a
    // phone's width they only push words onto the next line.
    out.push(inner.replace(/(\S) {3,}/g, "$1  "));
  }

  // No blank screen below the last thing written.
  while (out.length && out[out.length - 1].trim() === "") out.pop();
  // Nor a run of blank rows anywhere.
  return out.filter((l, i) => l.trim() !== "" || (i > 0 && out[i - 1].trim() !== ""));
}
