import { useEffect, useMemo, useRef, useState } from "react";
import { api, errMessage } from "../lib/api";
import { paneName } from "../lib/derive";
import { reportRepoResults } from "../lib/report";
import { useStore } from "../store";
import type {
  ChangedFile, CommitInfo, DiffScope, RepoBranchFacts, RepoCommits, ReviewComment, TaskView,
} from "../lib/types";
import { ChevronIcon } from "./icons";
import { Field, Modal, Spinner } from "./ui";
import { read, write } from "../lib/persist";

/** Same shape `send_review` builds — used when starting an agent with the notes. */
function reviewPrompt(comments: ReviewComment[]): string {
  let prompt = "Review feedback on your changes. Please address each point:\n\n";
  for (const c of comments) {
    const path = c.repo ? `${c.repo}/${c.path}` : c.path;
    prompt += `- ${path}:${c.line} — ${c.body.trim()}\n`;
    if (c.code?.trim()) prompt += `    (line reads: \`${c.code.trim()}\`)\n`;
  }
  return prompt;
}

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
      const m = /@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(raw);
      newLine = m ? Number(m[1]) : 0;
      inHunk = true;
      out.push({ kind: "hunk", text: raw, newLine: null });
    } else if (raw.startsWith("diff ")) {
      inHunk = false;
      out.push({ kind: "meta", text: raw, newLine: null });
    } else if (!inHunk) {
      // Before the first hunk it is all header: the names, the index, and
      // anything git adds — "old mode", "Binary files … differ" — which,
      // unlisted, was numbered from 0 as though it were the file.
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

/**
 * The box a note is typed into, holding its own text.
 *
 * In the diff's state, every keystroke re-rendered the whole diff — three
 * thousand rows — to change one textarea.
 */
function NoteEditor({ onAdd, onCancel }: { onAdd: (text: string) => void; onCancel: () => void }) {
  const [text, setText] = useState("");
  return (
    <div className="inline-comment">
      <textarea
        rows={3}
        autoFocus
        value={text}
        placeholder="What should the agent change here?"
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) onAdd(text);
          if (e.key === "Escape") onCancel();
        }}
      />
      <div className="actions">
        <button className="btn btn-sm btn-primary" onClick={() => onAdd(text)}>
          Add note
        </button>
        <button className="btn btn-sm" onClick={onCancel}>
          Cancel
        </button>
        <span style={{ color: "var(--dimmer)", fontSize: 11 }}>⌘↵ to add</span>
      </div>
    </div>
  );
}

export function DiffView({ task }: { task: TaskView }) {
  const fail = useStore((s) => s.fail);
  const toast = useStore((s) => s.toast);
  const refreshTasks = useStore((s) => s.refreshTasks);
  const refreshPanes = useStore((s) => s.refreshPanes);
  const setTab = useStore((s) => s.setTab);
  const agents = useStore((s) => s.agents);
  const allPanes = useStore((s) => s.panes);
  const agentPanes = useMemo(
    () => allPanes.filter((p) => p.task_id === task.id && p.kind === "agent" && p.running),
    [allPanes, task.id],
  );
  const installed = useMemo(() => agents.filter((a) => a.installed), [agents]);

  const [files, setFiles] = useState<ChangedFile[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  /**
   * The patch on screen and the file it belongs to. Kept apart from the
   * selection so a patch is never drawn under another file's name: a slower
   * answer for the last file used to land after the next one's, and a failed
   * fetch left the previous patch up — where a note then took the new
   * file's path with the old file's line.
   */
  const [patch, setPatch] = useState<{ key: string; text: string } | null>(null);
  /** Why there is no patch to draw, when there is a reason. */
  const [patchNote, setPatchNote] = useState<string | null>(null);
  const [drafts, setDrafts] = useState<Draft[]>([]);
  const [composing, setComposing] = useState<{ line: number; code: string } | null>(null);
  const [committing, setCommitting] = useState(false);
  const [commitBusy, setCommitBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [target, setTarget] = useState<string>("");
  /** No running agent: pick a CLI (and Start in) so Send can start one. */
  const [starting, setStarting] = useState(false);
  const [startAgentId, setStartAgentId] = useState("");
  const [startScope, setStartScope] = useState<string | null>(null);
  const [startingBusy, setStartingBusy] = useState(false);
  const [width, setWidth] = useState(() => read("diffWidth", 260));
  const [shut, setShut] = useState<Record<string, boolean>>({});
  // What "changed" means here. Uncommitted by default: it is what you are
  // holding and what git status agrees with. The whole branch is a different
  // and equally real question, and only worth asking when the branch was cut
  // for this work — which is not true of every worktree.
  const [scope, setScope] = useState<DiffScope>(() => read("diffScope", "uncommitted"));
  // A single commit on the branch, when set. Cleared on Uncommitted — that
  // view is about the working tree, not history.
  const [pin, setPin] = useState<{ checkoutId: string; sha: string } | null>(null);
  const [repoCommits, setRepoCommits] = useState<RepoCommits[]>([]);

  const multi = task.checkouts.length > 1;
  const current = files.find((f) => fileKey(f) === selected) ?? null;
  /** Null until measured: "0 commits, never pushed" is a claim, not a placeholder. */
  const [facts, setFacts] = useState<RepoBranchFacts[] | null>(null);

  // Flat list for the picker, newest first within each repo. Multi-repo rows
  // are prefixed so two identical subjects stay distinguishable.
  const commitOptions = useMemo(() => {
    const rows: { checkoutId: string; repo: string; commit: CommitInfo }[] = [];
    for (const r of repoCommits) {
      for (const c of r.commits) {
        rows.push({ checkoutId: r.checkout_id, repo: r.repo, commit: c });
      }
    }
    return rows;
  }, [repoCommits]);

  const pinIndex = pin
    ? commitOptions.findIndex((r) => r.checkoutId === pin.checkoutId && r.commit.sha === pin.sha)
    : -1;
  const pinned = pinIndex >= 0 ? commitOptions[pinIndex] : null;

  // Only the latest list lands. Uncommitted → Whole branch → Uncommitted
  // sent three, and the slow branch one arriving last filled the Uncommitted
  // tab with the whole branch.
  const loadSeq = useRef(0);
  async function load() {
    const asked = ++loadSeq.current;
    try {
      const list = await api.diffFiles(task.id, scope, pin);
      if (asked !== loadSeq.current) return;
      setFiles(list);
      setSelected((cur) =>
        cur && list.some((f) => fileKey(f) === cur) ? cur : (list[0] ? fileKey(list[0]) : null),
      );
    } catch (e) {
      if (asked === loadSeq.current) fail(e);
    }
  }

  // Reloaded when the task's own count of changed files moves, which the
  // sidebar polls every few seconds: an agent that just saved a file should
  // show up here without a hand on the Refresh button. A pinned commit is
  // history, so a working-tree save does not change it — but Refresh still
  // does, and so does switching pin/scope.
  const changedCount = task.checkouts.reduce((n, c) => n + c.changed, 0);
  useEffect(() => { void load(); /* eslint-disable-next-line */ }, [task.id, scope, pin, changedCount]);

  // Commit list for the picker. Refreshed with the file list so a new commit
  // from Commit… shows up without leaving the tab.
  useEffect(() => {
    if (scope !== "branch") {
      setRepoCommits([]);
      return;
    }
    api.taskCommits(task.id).then(setRepoCommits).catch(() => setRepoCommits([]));
  }, [task.id, scope, changedCount, files.length]);

  // What the numbers are measured against. Asked of git rather than worked out
  // from the file list, and reloaded with it so the two always agree. Only
  // where they are shown: it is several git calls per repo, and it ran on
  // every refresh of the Uncommitted list, which never reads it.
  useEffect(() => { setFacts(null); }, [task.id]);
  useEffect(() => {
    if (scope !== "branch") return;
    let current = true;
    api.taskBranchFacts(task.id)
      .then((f) => { if (current) setFacts(f); })
      .catch(() => { if (current) setFacts([]); });
    return () => { current = false; };
  }, [task.id, scope, files]);

  // Keyed on the list as well as the selection: a reload that keeps the same
  // file selected still has to fetch its patch again, or Refresh updates the
  // numbers in the list beside a diff that has not moved.
  const currentKey = current ? fileKey(current) : null;
  useEffect(() => {
    setPatchNote(null);
    if (!current || !currentKey) { setPatch(null); return; }
    // Not asked for: an untracked file listed as binary is either not text or
    // too big to read on every refresh, and reading it whole was how opening
    // this tab on a large log froze the window.
    if (current.binary && current.origin === "untracked") {
      setPatch(null);
      setPatchNote("Binary, or too large to show as a diff.");
      return;
    }
    let live = true;
    api.diffFile(current.checkout_id, current.path, scope, pin?.sha ?? null)
      .then((text) => { if (live) setPatch({ key: currentKey, text }); })
      .catch((e) => {
        if (!live) return;
        setPatch(null);
        setPatchNote(errMessage(e));
      });
    return () => { live = false; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentKey, scope, pin?.sha, files]);

  function pickScope(next: DiffScope) {
    setScope(next);
    write("diffScope", next);
    if (next === "uncommitted") setPin(null);
  }

  function pickCommit(value: string) {
    if (!value) {
      setPin(null);
      return;
    }
    const colon = value.indexOf(":");
    if (colon <= 0) return;
    const checkoutId = value.slice(0, colon);
    const sha = value.slice(colon + 1);
    if (!checkoutId || !sha) return;
    setPin({ checkoutId, sha });
    if (scope !== "branch") {
      setScope("branch");
      write("diffScope", "branch");
    }
  }

  function stepCommit(delta: number) {
    // Newest first: +1 walks older, −1 walks newer (and off the end clears the pin).
    if (commitOptions.length === 0) return;
    const at = pinIndex < 0 ? (delta > 0 ? -1 : 0) : pinIndex;
    const next = at + delta;
    if (next < 0) {
      setPin(null);
      return;
    }
    if (next >= commitOptions.length) return;
    const row = commitOptions[next];
    setPin({ checkoutId: row.checkoutId, sha: row.commit.sha });
  }

  useEffect(() => {
    if (!target || !agentPanes.some((p) => p.id === target)) {
      setTarget(agentPanes[0]?.id ?? "");
    }
  }, [agentPanes, target]);
  const targetPane = agentPanes.find((p) => p.id === target);
  const targetName = targetPane ? paneName(targetPane) : "the selected agent";

  useEffect(() => {
    if (!startAgentId || !installed.some((a) => a.id === startAgentId)) {
      setStartAgentId(installed[0]?.id ?? "");
    }
  }, [installed, startAgentId]);

  const shownPatch = patch && patch.key === currentKey ? patch.text : "";
  const parsed = useMemo(() => parseDiff(shownPatch), [shownPatch]);
  // Every line is a div, and a div per line of a generated file is what makes
  // opening this tab stutter. Showing the first few thousand keeps it instant;
  // anything past that is not being read line by line anyway.
  const [wholeFile, setWholeFile] = useState(false);
  // A note half written belongs to the line it was opened on. Keyed by line
  // number alone, it followed to the next file and was saved against that
  // file, with the first file's code quoted under it.
  useEffect(() => { setWholeFile(false); setComposing(null); }, [selected]);
  const lines = useMemo(
    () => (wholeFile ? parsed : parsed.slice(0, LINE_BUDGET)),
    [parsed, wholeFile],
  );
  const hidden = parsed.length - lines.length;
  const fileDrafts = useMemo(
    () => drafts.filter((d) => selected && `${d.checkoutId}:${d.path}` === selected),
    [drafts, selected],
  );

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
      // Depth indent only — the chevron column is reserved on every row so a
      // file under a folder lines up with the folder's name, not its triangle.
      const pad = { paddingLeft: 4 + depth * 14 };
      if (!node.file) {
        const closed = shut[node.path] ?? false;
        return (
          <div key={`d:${node.path}`}>
            <div
              className="diff-dir"
              style={pad}
              onClick={() => setShut((c) => ({ ...c, [node.path]: !closed }))}
            >
              <span className={`chev${closed ? "" : " open"}`}><ChevronIcon /></span>
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
          <span className="chev-spacer" aria-hidden />
          <span className="p">{node.name}</span>
          <span className="n" style={{ color: "var(--green)" }}>+{f.additions}</span>
          <span className="n" style={{ color: "var(--red)" }}>-{f.deletions}</span>
        </div>
      );
    });
  }

  function addDraft(text: string) {
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
    setComposing(null);
  }
  // Read through a ref so the rows below can be memoised without holding an
  // old `current` or `composing`.
  const addDraftRef = useRef(addDraft);
  addDraftRef.current = addDraft;

  function commentsFor(checkoutId: string | null): ReviewComment[] {
    return drafts.map((d) => ({
      // Only qualify the path when the agent is not already inside that repo.
      repo: checkoutId === d.checkoutId ? null : d.repo,
      path: d.path,
      line: d.line,
      body: d.body,
      code: d.code,
    }));
  }

  async function send() {
    if (drafts.length === 0) return;
    const pane = agentPanes.find((p) => p.id === target);
    if (pane) {
      try {
        const n = drafts.length;
        await api.sendReview(pane.id, commentsFor(pane.checkout_id));
        setDrafts([]);
        toast("success", `Sent ${n} note${n === 1 ? "" : "s"} to ${pane.title}.`);
      } catch (e) {
        fail(e);
      }
      return;
    }
    // Notes stay queued: open a picker instead of failing after the work.
    if (installed.length === 0) {
      toast("error", "No agent CLI is installed — install one, then send these notes.");
      return;
    }
    setStartScope(null);
    setStarting(true);
  }

  async function startAndSend() {
    if (!startAgentId || drafts.length === 0) return;
    setStartingBusy(true);
    try {
      const n = drafts.length;
      const pane = await api.spawnAgent(
        task.id,
        startAgentId,
        startScope,
        reviewPrompt(commentsFor(startScope)),
      );
      setDrafts([]);
      setStarting(false);
      await refreshPanes();
      setTab("terminals");
      toast("success", `Started ${pane.title} with ${n} note${n === 1 ? "" : "s"}.`);
    } catch (e) {
      fail(e);
    } finally {
      setStartingBusy(false);
    }
  }

  /// A commit runs the repo's own pre-commit hooks, which can be a whole lint
  /// pass, and in a multi-repo task it runs them once per repo. Without a busy
  /// state the dialog just sat there with the button still live, inviting a
  /// second press while the first was still in the hooks.
  async function commit() {
    setCommitBusy(true);
    try {
      const results = await api.commitTask(task.id, message.trim());
      reportRepoResults(toast, results, "Committed");
      // A hook that fails comes back as a result, not an error. Closing the
      // dialog then threw the message away with the repo still uncommitted.
      if (results.every((r) => r.ok)) {
        setCommitting(false);
        setMessage("");
      }
      await Promise.all([load(), refreshTasks()]);
    } catch (e) {
      fail(e);
    } finally {
      setCommitBusy(false);
    }
  }

  // The scope switch lives in the tray under the diff, and the tray is drawn
  // in both states: an empty "uncommitted" view is exactly when someone wants
  // to flip to the whole branch and see what was committed.
  /**
   * What this view is comparing, said out loud.
   *
   * The counts above a diff mean nothing without their baseline: "2 files" is
   * two files *against what*. Worse, a worktree with no recorded branch point
   * falls back to the merge base, which credits this branch with every commit
   * that arrived from anywhere else — which is how a two-line edit comes to
   * list a hundred and twenty-nine files. That distinction is now on screen
   * rather than buried in the difference between two similar numbers.
   */
  const summary = (() => {
    const adds = files.reduce((n, f) => n + f.additions, 0);
    const dels = files.reduce((n, f) => n + f.deletions, 0);

    if (scope === "uncommitted") {
      return {
        what: "against the last commit — written, but not committed yet",
        detail: "This is what `git status` shows: the working tree, including untracked files.",
        label: "Uncommitted",
        adds,
        dels,
      };
    }

    if (pinned) {
      const where = multi ? `${pinned.repo} · ` : "";
      return {
        what: `${where}${pinned.commit.short} — ${pinned.commit.subject}`,
        detail: "Files changed in this one commit. Use the arrows or the menu to move through the branch.",
        label: "Commit",
        adds,
        dels,
      };
    }

    if (facts === null) {
      return {
        what: "measuring the branch…",
        detail: "Asking git where this branch started and what it holds.",
        label: "Whole branch",
        adds,
        dels,
      };
    }
    const commits = facts.reduce((n, f) => n + f.commits, 0);
    const unpushed = facts.reduce((n, f) => n + f.unpushed, 0);
    const guessed = facts.filter((f) => !f.baseline_recorded);
    const one = facts.length === 1 ? facts[0] : null;

    const from = one
      ? one.baseline_recorded
        ? `since ${one.baseline}, where this branch left ${one.base}`
        : `since ${one.baseline}, the merge base with ${one.base}`
      : `across ${facts.length} repositories`;

    const pushed = facts.every((f) => !f.has_remote)
      ? "never pushed"
      : unpushed > 0
        ? `${unpushed} not pushed`
        : "all pushed";

    return {
      what: `${commits} commit${commits === 1 ? "" : "s"} ${from} · ${pushed}`,
      detail: guessed.length
        ? `No branch point was recorded for ${
            one ? "this worktree" : `${guessed.length} of these repositories`
          }, so the comparison falls back to the merge base — commits merged in from elsewhere are counted here too.`
        : "Measured from the commit this worktree was created at, so a moving base branch cannot inflate it.",
      label: "Whole branch",
      adds,
      dels,
    };
  })();

  const summaryBar = (
    <div className="diff-summary" title={summary.detail}>
      <b>{summary.label}</b>
      <span style={{ color: "var(--dim)" }}>{summary.what}</span>
      <div className="spacer" />
      <span style={{ color: "var(--dim)" }}>
        {files.length} file{files.length === 1 ? "" : "s"}
      </span>
      <span style={{ color: "var(--green)" }}>+{summary.adds}</span>
      <span style={{ color: "var(--red)" }}>−{summary.dels}</span>
    </div>
  );

  const scopeTabs = (
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
          onClick={() => pickScope(v)}
        >
          {v === "uncommitted" ? "Uncommitted" : "Whole branch"}
        </button>
      ))}
    </div>
  );

  const commitPicker = scope === "branch" && commitOptions.length > 0 ? (
    <div className="diff-commit-picker" title="Show the files changed in one commit">
      <button
        className="btn btn-sm"
        disabled={pinIndex >= commitOptions.length - 1}
        onClick={() => stepCommit(1)}
        title="Older commit"
      >
        ‹
      </button>
      <select
        value={pin ? `${pin.checkoutId}:${pin.sha}` : ""}
        onChange={(e) => pickCommit(e.target.value)}
      >
        <option value="">All since branch point</option>
        {commitOptions.map((r) => (
          <option
            key={`${r.checkoutId}:${r.commit.sha}`}
            value={`${r.checkoutId}:${r.commit.sha}`}
          >
            {multi ? `${r.repo} · ` : ""}
            {r.commit.short} — {r.commit.subject}
          </option>
        ))}
      </select>
      <button
        className="btn btn-sm"
        disabled={pinIndex < 0}
        onClick={() => stepCommit(-1)}
        title="Newer commit"
      >
        ›
      </button>
    </div>
  ) : null;

  // The rows, memoised: typing in a note or the commit message, a poll, or
  // anything else that redraws this view no longer rebuilds thousands of them.
  const composingLine = composing?.line ?? null;
  const linesView = useMemo(
    () =>
      lines.map((l, i) => {
        const anchored = l.newLine !== null
          ? fileDrafts.filter((d) => d.line === l.newLine)
          : [];
        const isComposing = composingLine === l.newLine && l.newLine !== null;
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
              <NoteEditor
                onAdd={(text) => addDraftRef.current(text)}
                onCancel={() => setComposing(null)}
              />
            )}
          </div>
        );
      }),
    [lines, fileDrafts, composingLine],
  );

  // A worktree git cannot read has no changes to list, which looked exactly
  // like a clean one: "No changes yet" over seven task folders whose files
  // were all still there.
  const unlinked = task.checkouts.filter((c) => c.broken);
  const unlinkedBanner = unlinked.length > 0 && (
    <div className="limit-banner unlinked-banner">
      <span>
        <b>Git cannot read {unlinked.map((c) => c.project_name).join(", ")}</b>, so{" "}
        {unlinked.length === 1 ? "its" : "their"} changes are not listed here. The files are
        still in the task folder. {unlinked[0].broken}
      </span>
    </div>
  );

  if (files.length === 0) {
    const where = multi ? "any of the task's repos" : "this worktree";
    return (
      <>
        {summaryBar}
        {unlinkedBanner}
        <div className="empty">
          <h2>
            {unlinked.length === task.checkouts.length
              ? "Nothing git can read"
              : pin
                ? "Nothing in this commit"
                : scope === "uncommitted"
                  ? "Nothing uncommitted"
                  : "No changes yet"}
          </h2>
          <p>
            {unlinked.length === task.checkouts.length
              ? "This is not a clean worktree: git has lost track of it, usually because its repository was deleted or cloned again. The files are still in the task folder. Once the folder is linked to its repository again, its changes show up here."
              : pin
                ? "That commit did not touch any files (or they are no longer in this worktree)."
                : scope === "uncommitted"
                  ? `The working tree is clean in ${where}. Switch to Whole branch to see what has been committed.`
                  : `Nothing differs from where this branch started in ${where}. Once an agent edits files they show up here.`}
          </p>
          <button className="btn" onClick={() => void load()}>Refresh</button>
        </div>
        <div className="review-tray">
          <div className="review-tray-group" title="What the list is measuring">
            {scopeTabs}
          </div>
          {commitPicker}
        </div>
      </>
    );
  }

  return (
    <>
      {summaryBar}
      {unlinkedBanner}
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

        <div className="diff-main">
          {/*
            The banners sit outside the scrolling region. Inside it they would
            slide away sideways when a long line is scrolled, which is the one
            direction a header has no business moving in.
          */}
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
          {patchNote && <div className="diff-more">{patchNote}</div>}
          <div className="diff-body">
          <div className="diff-lines">
          {linesView}
          </div>
          </div>
        </div>
      </div>

      <div className="review-tray">
        <div className="review-tray-group" title="What the list is measuring">
          {scopeTabs}
        </div>
        {commitPicker}
        <span className="review-tray-hint">
          {drafts.length === 0
            ? "Click a line number to leave a note"
            : agentPanes.length === 0
              ? `${drafts.length} note${drafts.length === 1 ? "" : "s"} queued · no agent running — Send will start one`
              : `${drafts.length} note${drafts.length === 1 ? "" : "s"} queued`}
        </span>
        <div className="spacer" />
        <div className="review-tray-actions">
          {agentPanes.length > 1 && (
            <label className="review-target">
              <span>Send notes to</span>
              <select
                value={target}
                onChange={(e) => setTarget(e.target.value)}
              >
                {agentPanes.map((p) => (
                  <option key={p.id} value={p.id}>{paneName(p)}</option>
                ))}
              </select>
            </label>
          )}
          <button className="btn btn-sm" onClick={() => void load()}>Refresh</button>
          <button className="btn btn-sm" onClick={() => setCommitting(true)}>Commit…</button>
          <button
            className="btn btn-sm btn-primary"
            disabled={drafts.length === 0}
            onClick={() => void send()}
            title={
              drafts.length === 0
                ? "Add notes on line numbers first"
                : agentPanes.length === 0
                  ? installed.length === 0
                    ? "Install an agent CLI first"
                    : "No agent running — pick one to start with these notes"
                  : agentPanes.length > 1
                    ? `Send queued notes to ${targetName}`
                    : "Send queued notes to the agent"
            }
          >
            {agentPanes.length === 0 && drafts.length > 0 ? "Start agent & send" : "Send to agent"}
            {drafts.length > 0 && <span className="badge">{drafts.length}</span>}
          </button>
        </div>
      </div>

      {starting && (
        <Modal
          title="Start an agent for these notes"
          onClose={() => !startingBusy && setStarting(false)}
          footer={
            <>
              <button className="btn" disabled={startingBusy} onClick={() => setStarting(false)}>
                Cancel
              </button>
              <button
                className="btn btn-primary"
                disabled={startingBusy || !startAgentId}
                onClick={() => void startAndSend()}
              >
                {startingBusy ? "Starting…" : "Start & send"}
              </button>
            </>
          }
        >
          <p style={{ marginTop: 0, color: "var(--dim)", fontSize: 13, lineHeight: 1.5 }}>
            Nothing is running in this task. Pick an agent to start — your{" "}
            {drafts.length} queued note{drafts.length === 1 ? "" : "s"} become its opening prompt.
          </p>
          <Field label="Agent">
            <select
              value={startAgentId}
              onChange={(e) => setStartAgentId(e.target.value)}
              disabled={startingBusy}
            >
              {installed.map((a) => (
                <option key={a.id} value={a.id}>{a.name}</option>
              ))}
            </select>
          </Field>
          <Field
            label="Start in"
            hint="The task folder sees every repo as a sibling folder. Pinning to one hides the others from the agent."
          >
            <select
              value={startScope ?? ""}
              onChange={(e) => setStartScope(e.target.value || null)}
              disabled={startingBusy}
            >
              <option value="">
                The task folder
                {task.checkouts.length > 1 ? ` — all ${task.checkouts.length} repos` : ""}
              </option>
              {task.checkouts.map((c) => (
                <option key={c.id} value={c.id}>Only {c.project_name}</option>
              ))}
            </select>
          </Field>
        </Modal>
      )}

      {committing && (
        <Modal
          title={multi ? "Commit every repo with changes" : "Commit all changes"}
          onClose={() => { if (!commitBusy) setCommitting(false); }}
          footer={
            <>
              <button
                className="btn"
                disabled={commitBusy}
                onClick={() => setCommitting(false)}
              >
                Cancel
              </button>
              <button
                className="btn btn-primary"
                disabled={commitBusy || !message.trim()}
                onClick={() => void commit()}
              >
                {commitBusy ? (
                  <span className="btn-busy"><Spinner />Committing…</span>
                ) : (
                  "Commit"
                )}
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
