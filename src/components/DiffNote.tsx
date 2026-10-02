import { useEffect, useRef, useState } from "react";
import { useStore, type ReviewNote } from "../store";
import { Markdown } from "./Markdown";
import { Spinner } from "./ui";

const SEVERITY = { bug: "del", risk: "warn", nit: "" } as const;

/**
 * `text` with a suggested change for `code` added at the end, as GitHub
 * writes one: on GitHub the author applies it in one click.
 */
function withSuggestion(text: string, code: string): string {
  const lead = text.trim() ? `${text.trimEnd()}\n\n` : "";
  return `${lead}\`\`\`suggestion\n${code}\n\`\`\``;
}

/**
 * A note's text being written: a new one, or one edited. "Suggest a change"
 * starts a suggestion from the line's code, to be edited into what it
 * should read.
 */
function NoteText({
  initial, code, placeholder, saveLabel, onSave, onCancel,
}: {
  initial: string;
  code: string;
  placeholder: string;
  saveLabel: string;
  onSave: (text: string) => void;
  onCancel: () => void;
}) {
  const [text, setText] = useState(initial);
  const box = useRef<HTMLTextAreaElement>(null);
  return (
    <>
      <textarea
        ref={box}
        rows={Math.min(12, Math.max(3, text.split("\n").length + 1))}
        autoFocus
        value={text}
        placeholder={placeholder}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) onSave(text);
          if (e.key === "Escape") onCancel();
        }}
      />
      <div className="actions">
        <button className="btn btn-sm btn-primary" onClick={() => onSave(text)}>{saveLabel}</button>
        <button className="btn btn-sm" onClick={onCancel}>Cancel</button>
        {code !== "" && !text.includes("```suggestion") && (
          <button
            className="btn btn-sm"
            title="Write what the line should read; the author can apply it from GitHub"
            onClick={() => {
              const next = withSuggestion(text, code);
              setText(next);
              // The line's code selected inside the block: typing replaces
              // it with what it should read.
              const end = next.length - "\n```".length;
              requestAnimationFrame(() => {
                box.current?.focus();
                box.current?.setSelectionRange(end - code.length, end);
              });
            }}
          >
            Suggest a change
          </button>
        )}
        <span style={{ color: "var(--dimmer)", fontSize: 11 }}>⌘↵ to {saveLabel.toLowerCase()}</span>
      </div>
    </>
  );
}

/**
 * The box a note is typed into, holding its own text.
 *
 * In the diff's state, every keystroke re-rendered the whole diff — three
 * thousand rows — to change one textarea.
 */
export function NoteEditor({
  onAdd, onCancel, placeholder, code,
}: { onAdd: (text: string) => void; onCancel: () => void; placeholder: string; code: string }) {
  return (
    <div className="inline-comment">
      <NoteText initial="" code={code} placeholder={placeholder} saveLabel="Add note" onSave={onAdd} onCancel={onCancel} />
    </div>
  );
}

/**
 * A note under a line of the Diff tab (DIFF-6). Yours is queued to send as
 * written. The reviewer's is a suggestion until kept: Keep queues it, Drop
 * throws it away, and only what is queued is ever sent.
 */
export function NoteCard({
  note,
  onKeep,
  onDrop,
  onEdit,
  where,
}: {
  note: ReviewNote;
  onKeep: () => void;
  onDrop: () => void;
  /** Reworded: a finding reworded is yours, and kept. */
  onEdit: (body: string) => void;
  /** Said when the note is not under its line: `L120`. */
  where?: string;
}) {
  const finding = note.by === "reviewer";
  const [editing, setEditing] = useState(false);
  // Said when it is not plain: a removed line, a range, or away from its line.
  const span = note.side === "LEFT"
    ? `removed L${note.line}`
    : note.startLine ? `L${note.startLine}–${note.line}` : where ? `L${note.line}` : null;
  if (editing) {
    return (
      <div className="inline-comment" data-note={note.id}>
        <NoteText
          initial={note.body}
          code={note.code}
          placeholder="What should the author know about this line?"
          saveLabel="Save"
          onSave={(body) => { if (body.trim()) onEdit(body.trim()); setEditing(false); }}
          onCancel={() => setEditing(false)}
        />
      </div>
    );
  }
  return (
    <div className={`inline-comment${finding && !note.kept ? " finding" : ""}`} data-note={note.id}>
      {(finding || span) && (
        <div className="note-head">
          {span && <span className="chip">{span}</span>}
          {finding && note.severity && <span className={`chip ${SEVERITY[note.severity]}`}>{note.severity}</span>}
          {finding && <span className="muted">{note.kept ? "reviewer, kept" : "reviewer"}</span>}
        </div>
      )}
      <div className="body"><Markdown text={note.body} /></div>
      <div className="actions">
        {finding && !note.kept && (
          <button className="btn btn-sm btn-primary" onClick={onKeep} title="Queue it to send with your notes">
            Keep
          </button>
        )}
        <button className="btn btn-sm" onClick={() => setEditing(true)} title={finding && !note.kept ? "Reword it, and keep it as yours" : "Reword it"}>
          Edit
        </button>
        <button className="btn btn-sm btn-danger" onClick={onDrop}>
          {finding && !note.kept ? "Drop" : "Remove"}
        </button>
      </div>
    </div>
  );
}

/** Seconds as the reviewer button counts them: `0:42`. */
function clock(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

/**
 * Starts the reviewer pass on the task's whole branch (DIFF-6), and says how
 * long one under way has run: it reads around the diff, and takes a minute
 * or more on a large change.
 */
export function ReviewerButton({ taskId }: { taskId: string }) {
  const state = useStore((s) => s.reviewer[taskId]);
  const run = useStore((s) => s.runReviewer);
  const model = useStore((s) => s.settings?.ui.reviewer_model ?? "sonnet");
  const running = state?.running ?? false;
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!running) return;
    // guard: allow poll — a clock on a run the user is watching, only while it runs
    const tick = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(tick);
  }, [running]);
  const last = state?.last;
  const title = running
    ? "The reviewer is reading the branch"
    : `A fresh ${model} run reads the whole branch, the commits and the ticket, and leaves notes for you to keep or drop.` +
      (last ? ` Last run found ${last.findings.length}.` : "");
  return (
    <button className="btn btn-sm" disabled={running} title={title} onClick={() => void run(taskId)}>
      {running ? (
        <span className="btn-busy"><Spinner />Reviewing… {clock(now - (state?.at ?? now))}</span>
      ) : last ? "Review with Claude again" : "Review with Claude"}
    </button>
  );
}
