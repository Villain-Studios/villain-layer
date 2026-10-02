/** A unified patch read for drawing, and the changed files as a tree: the Diff tab's pure parts. */
import type { ChangedFile } from "./types";

export type LineKind = "meta" | "hunk" | "add" | "del" | "ctx";

export interface DiffLine {
  kind: LineKind;
  text: string;
  newLine: number | null;
  /** In the old version of the file: where a removed line is commented on. */
  oldLine: number | null;
}

/** A line a note can be left on: a removed one on the old side, the rest on the new. */
export function lineOf(l: DiffLine): { side: "LEFT" | "RIGHT"; line: number } | null {
  if (l.kind === "del") return l.oldLine === null ? null : { side: "LEFT", line: l.oldLine };
  return l.newLine === null ? null : { side: "RIGHT", line: l.newLine };
}

/** Parse a unified patch, tracking both sides' line numbers for comment anchors. */
export function parseDiff(patch: string): DiffLine[] {
  const out: DiffLine[] = [];
  let newLine = 0;
  let oldLine = 0;
  // Whether the line belongs to a hunk or to the header before one. It
  // matters for `---` and `+++`: in the header they name the two files, but
  // inside a hunk a removed `-- SQL comment` or an added `++ counter` looks
  // exactly the same, and reading those as headers dropped them from the
  // numbering and drew them as metadata.
  let inHunk = false;
  // A patch ends in a newline, and the empty string after it was drawn as one
  // more numbered line that a note could be left on.
  const body = patch.endsWith("\n") ? patch.slice(0, -1) : patch;
  if (!body) return out;

  for (const raw of body.split("\n")) {
    if (raw.startsWith("@@")) {
      const m = /@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(raw);
      oldLine = m ? Number(m[1]) : 0;
      newLine = m ? Number(m[2]) : 0;
      inHunk = true;
      out.push({ kind: "hunk", text: raw, newLine: null, oldLine: null });
    } else if (raw.startsWith("diff ")) {
      inHunk = false;
      out.push({ kind: "meta", text: raw, newLine: null, oldLine: null });
    } else if (!inHunk) {
      // Before the first hunk it is all header: the names, the index, and
      // anything git adds — "old mode", "Binary files … differ" — which,
      // unlisted, was numbered from 0 as though it were the file.
      out.push({ kind: "meta", text: raw, newLine: null, oldLine: null });
    } else if (raw.startsWith("+")) {
      out.push({ kind: "add", text: raw, newLine: newLine++, oldLine: null });
    } else if (raw.startsWith("-")) {
      out.push({ kind: "del", text: raw, newLine: null, oldLine: oldLine++ });
    } else if (raw.startsWith("\\")) {
      out.push({ kind: "meta", text: raw, newLine: null, oldLine: null });
    } else {
      out.push({ kind: "ctx", text: raw, newLine: newLine++, oldLine: oldLine++ });
    }
  }
  return out;
}

/** A changed file, or a folder holding more of them. */
export interface Node {
  name: string;
  path: string;
  file?: ChangedFile;
  children: Node[];
}

/**
 * Files arranged by directory, with runs of single-child folders joined into
 * one row.
 *
 * A flat list of full paths is unreadable past a handful of files: every row is
 * ellipsised in the middle, and the part that differs is the part that gets
 * cut. Collapsing `src/app/auth/components` into a single row spends the width
 * on the names instead.
 */
export function toTree(files: ChangedFile[]): Node[] {
  const root: Node = { name: "", path: "", children: [] };

  for (const file of files) {
    let at = root;
    const parts = file.path.split("/");
    parts.forEach((part, i) => {
      const path = parts.slice(0, i + 1).join("/");
      const leaf = i === parts.length - 1;
      let next = at.children.find((c) => c.name === part && !c.file === !leaf);
      if (!next) {
        next = { name: part, path, children: [], ...(leaf ? { file } : {}) };
        at.children.push(next);
      }
      at = next;
    });
  }

  const squash = (node: Node): Node => {
    let here = node;
    while (!here.file && here.children.length === 1 && !here.children[0].file) {
      const only = here.children[0];
      here = { ...only, name: `${here.name}/${only.name}` };
    }
    return { ...here, children: here.children.map(squash) };
  };

  // Folders first, then files, each alphabetical — the order a file tree has
  // everywhere else.
  const sort = (nodes: Node[]): Node[] =>
    nodes
      .map((n) => ({ ...n, children: sort(n.children) }))
      .sort((a, b) =>
        !a.file === !b.file ? a.name.localeCompare(b.name) : a.file ? 1 : -1,
      );

  return sort(root.children.map(squash));
}
