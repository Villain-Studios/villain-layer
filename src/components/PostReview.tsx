import { useEffect, useRef, useState } from "react";
import { api } from "../lib/api";
import { useStore, type ReviewNote } from "../store";
import type { ReviewOf, ReviewVerdict } from "../lib/types";
import { Spinner } from "./ui";

const NO_NOTES: ReviewNote[] = [];

const VERDICTS: { id: ReviewVerdict; label: string; detail: string }[] = [
  { id: "comment", label: "Comment", detail: "Feedback, without a verdict" },
  { id: "approve", label: "Approve", detail: "Fine to merge" },
  { id: "request_changes", label: "Request changes", detail: "Not to merge before these are addressed" },
];

/** Where a note is, as the review lists it: `a.ts:12-16 (removed)`. */
function placeOf(n: ReviewNote): string {
  return `${n.path}:${n.startLine ? `${n.startLine}-${n.line}` : n.line}${n.side === "LEFT" ? " (removed)" : ""}`;
}

/**
 * Submit review, where GitHub has it (REV-11): a button in the review's
 * header counting the comments waiting, opening onto the verdict, the
 * summary and every comment with its line. Nothing reaches GitHub before
 * its Submit; findings not kept or dropped yet are said, and left out.
 */
export function SubmitReview({ taskId, review }: { taskId: string; review: ReviewOf }) {
  const fail = useStore((s) => s.fail);
  const toast = useStore((s) => s.toast);
  const dropNotes = useStore((s) => s.dropNotes);
  const showNextFinding = useStore((s) => s.showNextFinding);
  const all = useStore((s) => s.notes[taskId] ?? NO_NOTES);
  const notes = all.filter((n) => n.by === "you" || n.kept);
  const undecided = all.length - notes.length;
  const [open, setOpen] = useState(false);
  const [verdict, setVerdict] = useState<ReviewVerdict>("comment");
  const [body, setBody] = useState("");
  const [busy, setBusy] = useState(false);
  // A second click before the first answer would post the review twice.
  const sending = useRef(false);
  const panel = useRef<HTMLDivElement>(null);
  const empty = notes.length === 0 && !body.trim() && verdict !== "approve";

  // Shut by a click outside it, or Escape, as a dropdown is; never while
  // the review is being sent.
  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => {
      if (!busy && panel.current && !panel.current.contains(e.target as Node)) setOpen(false);
    };
    const key = (e: KeyboardEvent) => { if (e.key === "Escape" && !busy) setOpen(false); };
    document.addEventListener("mousedown", away);
    document.addEventListener("keydown", key);
    return () => {
      document.removeEventListener("mousedown", away);
      document.removeEventListener("keydown", key);
    };
  }, [open, busy]);

  async function post() {
    if (sending.current || empty) return;
    sending.current = true;
    setBusy(true);
    try {
      const sent = notes.map((n) => n.id);
      const posted = await api.githubPostReview(
        taskId,
        verdict,
        body,
        notes.map((n) => ({
          path: n.path, line: n.line, body: n.body, code: n.code, repo: null,
          side: n.side ?? null, start_line: n.startLine ?? null,
        })),
      );
      dropNotes(taskId, sent);
      setBody("");
      const where = posted.moved_all
        ? `${posted.in_body} note${posted.in_body === 1 ? "" : "s"} in its body: GitHub would not anchor them to lines`
        : [
            posted.inline && `${posted.inline} on lines`,
            posted.in_body && `${posted.in_body} in the body`,
          ].filter(Boolean).join(", ") || "no notes";
      toast(posted.moved_all ? "info" : "success", `Posted your review of ${review.repo}#${review.number}: ${where}.`);
      setOpen(false);
    } catch (e) {
      fail(e);
    } finally {
      sending.current = false;
      setBusy(false);
    }
  }

  return (
    <div className="submit-review" ref={panel}>
      <button
        className="btn btn-sm btn-primary"
        title={`Submit your review of ${review.repo}#${review.number}`}
        onClick={() => setOpen((o) => !o)}
      >
        Submit review{notes.length > 0 && <span className="badge">{notes.length}</span>}
      </button>
      {open && (
        <div className="submit-panel">
          <textarea
            rows={4}
            autoFocus
            value={body}
            disabled={busy}
            placeholder="Leave a comment (optional when there are line comments, or when approving)"
            onChange={(e) => setBody(e.target.value)}
          />
          <div className="verdicts">
            {VERDICTS.map((v) => (
              <label key={v.id} className={verdict === v.id ? "active" : ""}>
                <input type="radio" name="verdict" checked={verdict === v.id} disabled={busy} onChange={() => setVerdict(v.id)} />
                <span>{v.label}<span className="muted">{v.detail}</span></span>
              </label>
            ))}
          </div>
          <div className="submit-notes">
            <div className="muted">
              {notes.length === 0
                ? "No line comments: only the comment above and the verdict go."
                : `${notes.length} line comment${notes.length === 1 ? "" : "s"}, each on its line, pinned to the commit you reviewed. One outside the pull request's diff goes in the comment, under its line.`}
            </div>
            <ul className="post-notes">
              {notes.map((n) => (
                <li key={n.id}>
                  <code>{placeOf(n)}</code>
                  <span>{n.body}</span>
                </li>
              ))}
            </ul>
            {undecided > 0 && (
              <button className="link warn" onClick={() => { setOpen(false); showNextFinding(taskId); }}>
                {undecided} finding{undecided === 1 ? "" : "s"} not kept or dropped yet, and left out →
              </button>
            )}
          </div>
          <div className="submit-actions">
            <button className="btn btn-sm" disabled={busy} onClick={() => setOpen(false)}>Cancel</button>
            <button
              className="btn btn-sm btn-primary"
              disabled={busy || empty}
              title={empty ? "Write a comment or leave a line comment first" : `Post to ${review.url}`}
              onClick={() => void post()}
            >
              {busy ? <span className="btn-busy"><Spinner />Submitting…</span> : "Submit review"}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
