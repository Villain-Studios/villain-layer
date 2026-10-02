import { useEffect, useState } from "react";
import { useStore, type ReviewNote } from "../store";
import { Spinner } from "./ui";

const SEVERITY = { bug: "del", risk: "warn", nit: "" } as const;

/**
 * A note under a line of the Diff tab (DIFF-6). Yours is queued to send as
 * written. The reviewer's is a suggestion until kept: Keep queues it, Drop
 * throws it away, and only what is queued is ever sent.
 */
export function NoteCard({
  note,
  onKeep,
  onDrop,
  where,
}: {
  note: ReviewNote;
  onKeep: () => void;
  onDrop: () => void;
  /** Said when the note is not under its line: `L120`. */
  where?: string;
}) {
  const finding = note.by === "reviewer";
  return (
    <div className={`inline-comment${finding && !note.kept ? " finding" : ""}`} data-note={note.id}>
      {(finding || where) && (
        <div className="note-head">
          {where && <span className="chip">{where}</span>}
          {finding && note.severity && <span className={`chip ${SEVERITY[note.severity]}`}>{note.severity}</span>}
          {finding && <span className="muted">{note.kept ? "reviewer, kept" : "reviewer"}</span>}
        </div>
      )}
      <div className="body">{note.body}</div>
      <div className="actions">
        {finding && !note.kept && (
          <button className="btn btn-sm btn-primary" onClick={onKeep} title="Queue it to send with your notes">
            Keep
          </button>
        )}
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
