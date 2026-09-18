import { useEffect, useMemo, useState } from "react";
import { api } from "../lib/api";
import { useStore } from "../store";
import type { ChangedFile, RepoResult, TaskView } from "../lib/types";
import { Field, Modal } from "./ui";

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

  const multi = task.checkouts.length > 1;
  const current = files.find((f) => fileKey(f) === selected) ?? null;

  async function load() {
    try {
      const list = await api.diffFiles(task.id);
      setFiles(list);
      setSelected((cur) =>
        cur && list.some((f) => fileKey(f) === cur) ? cur : (list[0] ? fileKey(list[0]) : null),
      );
    } catch (e) {
      fail(e);
    }
  }

  useEffect(() => { void load(); /* eslint-disable-next-line */ }, [task.id]);

  useEffect(() => {
    if (!current) { setPatch(""); return; }
    api.diffFile(current.checkout_id, current.path).then(setPatch).catch(fail);
  }, [current?.checkout_id, current?.path, fail]);

  useEffect(() => {
    if (!target || !agentPanes.some((p) => p.id === target)) {
      setTarget(agentPanes[0]?.id ?? "");
    }
  }, [agentPanes, target]);

  const lines = useMemo(() => parseDiff(patch), [patch]);
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
        <div className="diff-files">
          {groups.map((g) => (
            <div key={g.checkout.id}>
              {multi && (
                <div className="diff-group">
                  {g.checkout.project_name}
                  <span style={{ color: "var(--dimmer)" }}> · {g.files.length}</span>
                </div>
              )}
              {g.files.map((f) => (
                <div
                  key={fileKey(f)}
                  className={`diff-file${fileKey(f) === selected ? " active" : ""}`}
                  onClick={() => setSelected(fileKey(f))}
                  title={`${f.repo}/${f.path}`}
                >
                  <span className="p">{f.path}</span>
                  <span className="n" style={{ color: "var(--green)" }}>+{f.additions}</span>
                  <span className="n" style={{ color: "var(--red)" }}>-{f.deletions}</span>
                </div>
              ))}
            </div>
          ))}
        </div>

        <div className="diff-body">
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
