/**
 * The open questions of a requirements file (SPEC-1, SPEC-19): each list
 * item under its Open questions heading, the answers drafted for it (the
 * items under it) and the user's own (an `Answer:` line under it, or one
 * starting `--`, which is how people answered before the tab had a place
 * for it). Read here rather than in the backend because the list beside the
 * editor follows every key; `spec.rs` (`answered`) reads the same shape for
 * the prompts.
 */
export interface Question {
  /** `Q-2`, or its place when the draft gave it none. */
  id: string;
  text: string;
  /** The answers drafted for it to pick from. */
  options: string[];
  answer: string | null;
}

interface Block {
  question: Question;
  /** Its lines: from its own to the last that belongs to it, exclusive. */
  start: number;
  end: number;
  answerLines: number[];
}

function listItem(trimmed: string): string | null {
  const m = trimmed.match(/^(?:[-*+]|\d+[.)])(?: +(.*)|$)/);
  return m ? (m[1] ?? "") : null;
}

/** `Answer: yes` (bullet and bold allowed) or `-- yes` as "yes". */
function answerOf(trimmed: string): string | null {
  const dashed = trimmed.startsWith("--");
  const rest = dashed ? trimmed.replace(/^-+\s*/, "") : (listItem(trimmed) ?? trimmed);
  const m = rest.match(/^\**answer\**\s*:\**\s*(.*)$/i);
  if (m) return m[1].trim();
  return dashed ? rest.trim() : null;
}

function blocks(text: string): Block[] {
  const found: Block[] = [];
  let inside = false;
  let fence = false;
  text.split("\n").forEach((line, i) => {
    const trimmed = line.trimStart();
    if (trimmed.startsWith("```")) {
      fence = !fence;
      return;
    }
    if (fence) return;
    if (trimmed.startsWith("#")) {
      inside = /open questions?/i.test(trimmed);
      return;
    }
    if (!inside || !trimmed.trim()) return;
    const last = found[found.length - 1];
    const answer = answerOf(trimmed);
    if (answer !== null) {
      if (!last) return;
      last.question.answer = answer || null;
      last.answerLines.push(i);
      last.end = i + 1;
      return;
    }
    const item = listItem(trimmed);
    const indent = line.length - trimmed.length;
    if (item !== null && indent < 2) {
      if (/^none\.?$/i.test(item.trim())) return;
      const id = item.match(/^\**(Q-\d+)\**\s*[:.\-–—]?\**\s*/i);
      const qid = id ? id[1].toUpperCase() : `Q-${found.length + 1}`;
      const rest = id ? item.slice(id[0].length) : item;
      found.push({ question: { id: qid, text: rest.trim(), options: [], answer: null }, start: i, end: i + 1, answerLines: [] });
      return;
    }
    if (!last) return;
    if (item !== null) {
      if (item.trim()) last.question.options.push(item.trim());
    } else {
      last.question.text += " " + trimmed.trim();
    }
    last.end = i + 1;
  });
  return found;
}

export function questions(text: string): Question[] {
  return blocks(text).map((b) => b.question);
}

/**
 * `text` with question `id` answered: its `Answer:` line replaced, or added
 * as the last line of the question. An empty answer takes it away.
 */
export function setAnswer(text: string, id: string, answer: string): string {
  const block = blocks(text).find((b) => b.question.id === id);
  if (!block) return text;
  const lines = text.split("\n");
  const own = lines
    .slice(block.start, block.end)
    .filter((_, j) => !block.answerLines.includes(block.start + j));
  const one = answer.replace(/\s*\n\s*/g, " ").trim();
  if (one) own.push(`  Answer: ${one}`);
  return [...lines.slice(0, block.start), ...own, ...lines.slice(block.end)].join("\n");
}

/** Whether `answer` is the drafted answer `option`, give or take case and spacing. */
export function sameAnswer(option: string, answer: string | null): boolean {
  const norm = (s: string) => s.trim().replace(/\s+/g, " ").toLowerCase();
  return answer !== null && norm(option) === norm(answer);
}

/**
 * What approving requirements that still hold questions puts at risk
 * (SPEC-21), or null when there are none.
 */
export function approveWarning(qs: Question[]): string | null {
  const open = qs.filter((q) => !q.answer).length;
  const answered = qs.length - open;
  const said: string[] = [];
  if (open > 0) {
    said.push(open === 1
      ? "1 open question has no answer. An agent working to these requirements will stop to ask it."
      : `${open} open questions have no answer. An agent working to these requirements will stop to ask them.`);
  }
  if (answered > 0) {
    said.push(`${answered === 1 ? "1 answer is" : `${answered} answers are`} not built into the requirements yet, and Check against spec reads only the requirements. Apply answers does that.`);
  }
  return said.length > 0 ? said.join(" ") : null;
}
