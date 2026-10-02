import { useRef, useState } from "react";
import { api } from "../lib/api";
import { useStore, type ReviewNote } from "../store";
import type { ReviewOf, ReviewVerdict } from "../lib/types";
import { Field, Modal, Spinner } from "./ui";

const VERDICTS: { id: ReviewVerdict; label: string; detail: string }[] = [
  { id: "comment", label: "Comment", detail: "Feedback, without a verdict" },
  { id: "approve", label: "Approve", detail: "Fine to merge" },
  { id: "request_changes", label: "Request changes", detail: "Not to merge before these are addressed" },
];

/**
 * Everything a review task's notes become on GitHub, shown before any of it
 * is sent (REV-11): the verdict, the summary, and every note with its line.
 * One click posts it all as one review; nothing reaches GitHub before.
 */
export function PostReview({
  taskId,
  review,
  notes,
  onClose,
}: {
  taskId: string;
  review: ReviewOf;
  notes: ReviewNote[];
  onClose: () => void;
}) {
  const fail = useStore((s) => s.fail);
  const toast = useStore((s) => s.toast);
  const dropNotes = useStore((s) => s.dropNotes);
  const [verdict, setVerdict] = useState<ReviewVerdict>("comment");
  const [body, setBody] = useState("");
  const [busy, setBusy] = useState(false);
  // A second click before the first answer would post the review twice.
  const sending = useRef(false);
  const empty = notes.length === 0 && !body.trim() && verdict !== "approve";

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
      const where = posted.moved_all
        ? `${posted.in_body} note${posted.in_body === 1 ? "" : "s"} in its body: GitHub would not anchor them to lines`
        : [
            posted.inline && `${posted.inline} on lines`,
            posted.in_body && `${posted.in_body} in the body`,
          ].filter(Boolean).join(", ") || "no notes";
      toast(posted.moved_all ? "info" : "success", `Posted your review of ${review.repo}#${review.number}: ${where}.`);
      onClose();
    } catch (e) {
      fail(e);
    } finally {
      sending.current = false;
      setBusy(false);
    }
  }

  return (
    <Modal
      title={`Review ${review.repo}#${review.number}`}
      onClose={() => !busy && onClose()}
      footer={
        <>
          <button className="btn" disabled={busy} onClick={onClose}>Cancel</button>
          <button
            className="btn btn-primary"
            disabled={busy || empty}
            title={empty ? "Write a summary or leave a note first" : `Post to ${review.url}`}
            onClick={() => void post()}
          >
            {busy ? <span className="btn-busy"><Spinner />Posting…</span> : "Post to GitHub"}
          </button>
        </>
      }
    >
      <Field label="Verdict">
        <div className="verdicts">
          {VERDICTS.map((v) => (
            <label key={v.id} className={verdict === v.id ? "active" : ""} title={v.detail}>
              <input
                type="radio"
                name="verdict"
                checked={verdict === v.id}
                disabled={busy}
                onChange={() => setVerdict(v.id)}
              />
              {v.label}
            </label>
          ))}
        </div>
      </Field>
      <Field label="Summary" hint="The review's body on GitHub. Optional when there are notes, or when approving.">
        <textarea rows={4} value={body} disabled={busy} onChange={(e) => setBody(e.target.value)} />
      </Field>
      <Field
        label={`Notes — ${notes.length}`}
        hint="Each goes as a comment on its line, pinned to the commit you reviewed. One on a line outside the pull request's diff goes in the body, under its file and line."
      >
        <ul className="post-notes">
          {notes.length === 0 && <li className="muted">No notes: only the verdict and summary are posted.</li>}
          {notes.map((n) => (
            <li key={n.id}>
              <code>
                {n.path}:{n.startLine ? `${n.startLine}-${n.line}` : n.line}
                {n.side === "LEFT" && " (removed)"}
              </code>
              <span>{n.body}</span>
            </li>
          ))}
        </ul>
      </Field>
    </Modal>
  );
}
