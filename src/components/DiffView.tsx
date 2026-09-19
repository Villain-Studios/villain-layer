import { useEffect, useMemo, useState } from "react";
import { api } from "../lib/api";
import { useStore } from "../store";
import type { ChangedFile, DiffScope, RepoResult, TaskView } from "../lib/types";
import { Field, Modal } from "./ui";
import { read, write } from "../lib/persist";

type LineKind = "meta" | "hunk" | "add" | "del" | "ctx";

interface DiffLine {
  kind: LineKind;
  text: string;
  newLine: number | null;
}

/** Parse a unified patch, tracking new-file line numbers for comment anchors. */
function parseDiff(patch: string): DiffLine[] {
  const out: DiffLine[] = [];
  let newLine = 0;

  for (const raw of patch.split("\n")) {
    if (raw.startsWith("@@")) {
      const m = /@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(raw);
      newLine = m ? Number(m[1]) : 0;
      out.push({ kind: "hunk", text: raw, newLine: null });
    } else if (
      raw.startsWith("diff ") || raw.startsWith("index ") ||
      raw.startsWith("--- ") || raw.startsWith("+++ ") ||
      raw.startsWith("new file") || raw.startsWith("deleted file") ||
      raw.startsWith("similarity ") || raw.startsWith("rename ")
    ) {
      out.push({ kind: "meta", text: raw, newLine: null });
    } else if (raw.startsWith("+")) {
      out.push({ kind: "add", text: raw, newLine: newLine++ });
    } else if (raw.startsWith("-")) {
      out.push({ kind: "del", text: raw, newLine: null });
    } else if (raw.startsWith("\\")) {
      out.push({ kind: "meta", text: raw, newLine: null });
    } else {
      out.push({ kind: "ctx", text: raw, newLine: newLine++ });
    }
  }
  return out;
}


/** A changed file, or a folder holding more of them. */
interface Node {
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
function toTree(files: ChangedFile[]): Node[] {
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

interface Draft {
  id: number;
  checkoutId: string;
  repo: string;
  path: string;
  line: number;
  body: string;
  code: string;
}

let draftSeq = 0;

const fileKey = (f: ChangedFile) => `${f.checkout_id}:${f.path}`;

/** Lines drawn before the rest is held back behind a click. */
const LINE_BUDGET = 3000;

export function DiffView({ task }: { task: TaskView }) {
  const fail = useStore((s) => s.fail);
  const toast = useStore((s) => s.toast);
  const refreshTasks = useStore((s) => s.refreshTasks);
  const allPanes = useStore((s) => s.panes);
  const agentPanes = useMemo(
    () => allPanes.filter((p) => p.task_id === task.id && p.kind === "agent" && p.running),
    [allPanes, task.id],
  );

  const [files, setFiles] = useState<ChangedFile[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [patch, setPatch] = useState("");
  const [drafts, setDrafts] = useState<Draft[]>([]);
  const [composing, setComposing] = useState<{ line: number; code: string } | null>(null);
  const [text, setText] = useState("");
  const [committing, setCommitting] = useState(false);
  const [message, setMessage] = useState("");
  const [target, setTarget] = useState<string>("");
  const [width, setWidth] = useState(() => read("diffWidth", 260));
  const [shut, setShut] = useState<Record<string, boolean>>({});
  // What "changed" means here. Uncommitted by default: it is what you are
  // holding and what git status agrees with. The whole branch is a different
  // and equally real question, and only worth asking when the branch was cut
  // for this work — which is not true of every worktree.
  const [scope, setScope] = useState<DiffScope>(() => read("diffScope", "uncommitted"));

  const multi = task.checkouts.length > 1;
  const current = files.find((f) => fileKey(f) === selected) ?? null;

  async function load() {
    try {
      const list = await api.diffFiles(task.id, scope);
      setFiles(list);
      setSelected((cur) =>
        cur && list.some((f) => fileKey(f) === cur) ? cur : (list[0] ? fileKey(list[0]) : null),
      );
    } catch (e) {
      fail(e);
    }
  }

  useEffect(() => { void load(); /* eslint-disable-next-line */ }, [task.id, scope]);

  useEffect(() => {
    if (!current) { setPatch(""); return; }
    api.diffFile(current.checkout_id, current.path, scope).then(setPatch).catch(fail);
  }, [current?.checkout_id, current?.path, scope, fail]);

  useEffect(() => {
    if (!target || !agentPanes.some((p) => p.id === target)) {
      setTarget(agentPanes[0]?.id ?? "");
    }
  }, [agentPanes, target]);

  const parsed = useMemo(() => parseDiff(patch), [patch]);
  // Every line is a div, and a div per line of a generated file is what makes
  // opening this tab stutter. Showing the first few thousand keeps it instant;
  // anything past that is not being read line by line anyway.
  const [wholeFile, setWholeFile] = useState(false);
  useEffect(() => { setWholeFile(false); }, [selected]);
  const lines = wholeFile ? parsed : parsed.slice(0, LINE_BUDGET);
  const hidden = parsed.length - lines.length;
  const fileDrafts = drafts.filter((d) => selected && `${d.checkoutId}:${d.path}` === selected);

  // Files grouped by repo, in the order the task's repos are listed.
  const groups = useMemo(() => {
    const byRepo = new Map<string, ChangedFile[]>();
    for (const f of files) {
      const list = byRepo.get(f.checkout_id) ?? [];
      list.push(f);
      byRepo.set(f.checkout_id, list);
    }
    return task.checkouts
      .map((c) => ({ checkout: c, files: byRepo.get(c.id) ?? [] }))
      .filter((g) => g.files.length > 0);
  }, [files, task.checkouts]);

  /// A tree row per node: folders fold away, files select.
  function renderNodes(nodes: Node[], depth: number) {
    return nodes.map((node) => {
      const pad = { paddingLeft: 6 + depth * 11 };
      if (!node.file) {
        const closed = shut[node.path] ?? false;
        return (
          <div key={`d:${node.path}`}>
            <div
              className="diff-dir"
              style={pad}
              onClick={() => setShut((c) => ({ ...c, [node.path]: !closed }))}
            >
              <span className={`chev${closed ? "" : " open"}`}>▶</span>
              <span className="p">{node.name}</span>
            </div>
            {!closed && renderNodes(node.children, depth + 1)}
          </div>
        );
      }
      const f = node.file;
      return (
        <div
          key={fileKey(f)}
          className={`diff-file${fileKey(f) === selected ? " active" : ""}`}
          style={pad}
          onClick={() => setSelected(fileKey(f))}
          title={`${f.repo}/${f.path}`}
        >
          <span className="p">{node.name}</span>
          <span className="n" style={{ color: "var(--green)" }}>+{f.additions}</span>
          <span className="n" style={{ color: "var(--red)" }}>-{f.deletions}</span>
        </div>
      );
    });
  }

  function addDraft() {
    if (!composing || !current || !text.trim()) { setComposing(null); return; }
    setDrafts((d) => [
      ...d,
      {
        id: ++draftSeq,
        checkoutId: current.checkout_id,
        repo: current.repo,
        path: current.path,
        line: composing.line,
        body: text.trim(),
        code: composing.code,
      },
    ]);
    setText("");
    setComposing(null);
  }

  async function send() {
    const pane = agentPanes.find((p) => p.id === target);
    if (!pane) {
      toast("error", "No running agent in this task to send review notes to.");
      return;
    }
    try {
      await api.sendReview(
        pane.id,
        drafts.map((d) => ({
          // Only qualify the path when the agent is not already inside that repo.
          repo: pane.checkout_id === d.checkoutId ? null : d.repo,
          path: d.path,
          line: d.line,
          body: d.body,
          code: d.code,
        })),
      );
      setDrafts([]);
      toast("success", `Sent ${drafts.length} note(s) to ${pane.title}.`);
    } catch (e) {
      fail(e);
    }
  }

  function report(results: RepoResult[], verb: string) {
    const ok = results.filter((r) => r.ok);
    const bad = results.filter((r) => !r.ok);
    if (bad.length) {
      toast("error", bad.map((r) => `${r.repo}: ${r.detail}`).join("\n"));
    }
    if (ok.length) {
      toast("success", `${verb} ${ok.map((r) => `${r.repo} (${r.detail})`).join(", ")}`);
    }
    if (!ok.length && !bad.length) {
      toast("info", "Nothing to do — no repository had changes.");
    }
  }

  async function commit() {
    try {
      report(await api.commitTask(task.id, message.trim()), "Committed");
      setCommitting(false);
      setMessage("");
      await Promise.all([load(), refreshTasks()]);
    } catch (e) {
      fail(e);
    }
  }

  if (files.length === 0) {
    return (
      <div className="empty">
        <h2>No changes yet</h2>
        <p>
          Nothing differs from the base branch in {multi ? "any of the task's repos" : "this worktree"}.
          Once an agent edits files they show up here.
        </p>
        <button className="btn" onClick={() => void load()}>Refresh</button>
      </div>
    );
  }

  return (
    <>
      <div className="diff">
        <div className="diff-files" style={{ width }}>
          {groups.map((g) => (
            <div key={g.checkout.id}>
              {multi && (
                <div className="diff-group">
                  {g.checkout.project_name}
                  <span style={{ color: "var(--dimmer)" }}> · {g.files.length}</span>
                </div>
              )}
              {renderNodes(toTree(g.files), 0)}
            </div>
          ))}
        </div>

        <div
          className="diff-grip"
          title="Drag to resize"
          onMouseDown={(e) => {
            e.preventDefault();
            const startX = e.clientX;
            const startWidth = width;
            const move = (m: MouseEvent) => {
              // Clamped so the list cannot be dragged away entirely or take
              // the whole pane; the diff is still the point of this view.
              const next = Math.min(640, Math.max(150, startWidth + m.clientX - startX));
              setWidth(next);
            };
            const done = () => {
              window.removeEventListener("mousemove", move);
              window.removeEventListener("mouseup", done);
              setWidth((w) => { write("diffWidth", w); return w; });
            };
            window.addEventListener("mousemove", move);
            window.addEventListener("mouseup", done);
          }}
        />

        <div className="diff-body">
          {hidden > 0 && (
            <div className="diff-more">
              Showing the first {LINE_BUDGET.toLocaleString()} lines.
              <button className="btn btn-sm" onClick={() => setWholeFile(true)}>
                Show all {parsed.length.toLocaleString()}
              </button>
            </div>
          )}
          {multi && current && (
            <div className="diff-repo-banner">
              {current.repo} / {current.path}
            </div>
          )}
          {lines.map((l, i) => {
            const anchored = l.newLine !== null
              ? fileDrafts.filter((d) => d.line === l.newLine)
              : [];
            const isComposing = composing?.line === l.newLine && l.newLine !== null;
            return (
              <div key={i}>
                <div
                  className={
                    "diff-line " +
                    (l.kind === "add" ? "add" : l.kind === "del" ? "del" :
                      l.kind === "hunk" ? "hunk" : l.kind === "meta" ? "meta" : "") +
                    (anchored.length ? " commented" : "")
                  }
                >
                  <span
                    className="ln"
                    onClick={() => {
                      if (l.newLine === null) return;
                      setComposing({ line: l.newLine, code: l.text.slice(1) });
                      setText("");
                    }}
                  >
                    {l.newLine ?? ""}
                  </span>
                  <span className="tx">{l.text || " "}</span>
                </div>

                {anchored.map((d) => (
                  <div key={d.id} className="inline-comment">
                    <div className="body">{d.body}</div>
                    <div className="actions">
                      <button
                        className="btn btn-sm btn-danger"
                        onClick={() => setDrafts((all) => all.filter((x) => x.id !== d.id))}
                      >
                        Remove
                      </button>
                    </div>
                  </div>
                ))}

                {isComposing && (
                  <div className="inline-comment">
                    <textarea
                      rows={3}
                      autoFocus
                      value={text}
                      placeholder="What should the agent change here?"
                      onChange={(e) => setText(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) addDraft();
                        if (e.key === "Escape") setComposing(null);
                      }}
                    />
                    <div className="actions">
                      <button className="btn btn-sm btn-primary" onClick={addDraft}>
                        Add note
                      </button>
                      <button className="btn btn-sm" onClick={() => setComposing(null)}>
                        Cancel
                      </button>
                      <span style={{ color: "var(--dimmer)", fontSize: 11 }}>⌘↵ to add</span>
                    </div>
                  </div>
                )}
              </div>
            );
          })}
        </div>
      </div>

      <div className="review-tray">
        <div className="subtabs">
          {(["uncommitted", "branch"] as DiffScope[]).map((v) => (
            <button
              key={v}
              className={scope === v ? "active" : ""}
              title={
                v === "uncommitted"
                  ? "Everything not yet committed — what git status shows"
                  : "Everything since this worktree was created, committed or not"
              }
              onClick={() => { setScope(v); write("diffScope", v); }}
            >
              {v === "uncommitted" ? "Uncommitted" : "Whole branch"}
            </button>
          ))}
        </div>
        <span style={{ color: "var(--dim)" }}>
          {drafts.length === 0
            ? "Click a line number to leave a note for the agent."
            : `${drafts.length} note${drafts.length === 1 ? "" : "s"} queued across ${
                new Set(drafts.map((d) => d.repo)).size
              } repo(s)`}
        </span>
        <div className="spacer" />
        {agentPanes.length > 1 && (
          <select
            style={{ width: "auto" }}
            value={target}
            onChange={(e) => setTarget(e.target.value)}
          >
            {agentPanes.map((p) => (
              <option key={p.id} value={p.id}>{p.title}</option>
            ))}
          </select>
        )}
        <button className="btn btn-sm" onClick={() => void load()}>Refresh</button>
        <button className="btn btn-sm" onClick={() => setCommitting(true)}>Commit…</button>
        <button
          className="btn btn-sm btn-primary"
          disabled={drafts.length === 0}
          onClick={() => void send()}
        >
          Send to agent
        </button>
      </div>

      {committing && (
        <Modal
          title={multi ? "Commit every repo with changes" : "Commit all changes"}
          onClose={() => setCommitting(false)}
          footer={
            <>
              <button className="btn" onClick={() => setCommitting(false)}>Cancel</button>
              <button
                className="btn btn-primary"
                disabled={!message.trim()}
                onClick={() => void commit()}
              >
                Commit
              </button>
            </>
          }
        >
          <Field
            label="Message"
            hint={
              multi
                ? `Stages and commits in each of the ${groups.length} repos that changed, under this one message.`
                : "Stages every change in the worktree, then commits."
            }
          >
            <textarea
              rows={4}
              autoFocus
              value={message}
              onChange={(e) => setMessage(e.target.value)}
              placeholder={
                task.issue_key
                  ? `${task.issue_key}: ${task.name.replace(/^\S+\s/, "")}`
                  : task.name
              }
            />
          </Field>
          {multi && (
            <div className="muted">
              {groups.map((g) => g.checkout.project_name).join(", ")}
            </div>
          )}
        </Modal>
      )}
    </>
  );
}
