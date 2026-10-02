import { useEffect, useMemo, useState } from "react";
import { api } from "../lib/api";
import { useStore, type ReviewNote } from "../store";
import type { RepoOutgoing } from "../lib/types";
import { ReviewerButton } from "./DiffNote";
import { Spinner } from "./ui";

const NO_NOTES: ReviewNote[] = [];
const KIND_TONE = { debug: "del", "focused test": "del", "conflict marker": "del", todo: "warn" } as const;

/**
 * What Open pull requests is about to push, looked at first (PR-11): per
 * repo the commits going out and the leftovers in their added lines, and
 * whether the reviewer pass has read this code. Advice, never a gate:
 * `onFlags` says how many things deserve a second look, and the dialog's
 * button says "anyway" when there are some.
 */
export function Outgoing({ taskId, onFlags }: { taskId: string; onFlags: (n: number) => void }) {
  const fail = useStore((s) => s.fail);
  const reviewer = useStore((s) => s.reviewer[taskId]);
  const notes = useStore((s) => s.notes[taskId] ?? NO_NOTES);
  const [rows, setRows] = useState<RepoOutgoing[] | null>(null);

  useEffect(() => {
    let current = true;
    api.taskOutgoing(taskId)
      .then((r) => { if (current) setRows(r); })
      .catch((e) => { if (current) { setRows([]); fail(e); } });
    return () => { current = false; };
  }, [taskId, fail]);

  const going = useMemo(() => (rows ?? []).filter((r) => r.commits.length > 0 || r.error), [rows]);
  const leftovers = going.reduce((n, r) => n + r.leftovers.length, 0);
  const undecided = notes.filter((n) => n.by === "reviewer" && !n.kept).length;
  const unsent = notes.filter((n) => n.by === "you" || n.kept).length;
  // Of the code going out now: every repo's HEAD is the one it read.
  const last = reviewer?.last ?? null;
  const current = !!last && going.every((r) => last.heads.some((h) => h.checkout_id === r.checkout_id && h.head === r.head));

  useEffect(() => { onFlags(leftovers + undecided + unsent); }, [leftovers, undecided, unsent, onFlags]);

  if (rows === null) {
    return <div className="outgoing muted"><Spinner /> Looking at what this would push…</div>;
  }
  if (going.length === 0) return null;

  return (
    <div className="outgoing">
      <div className="outgoing-head">
        <b>Before it goes</b>
        <span className="muted">
          {reviewer?.running
            ? "the reviewer is reading it"
            : !last
              ? "not reviewed"
              : current
                ? `reviewed at these commits${last.findings.length ? "" : ", nothing found"}`
                : "reviewed, but commits came after"}
          {undecided > 0 && ` · ${undecided} finding${undecided === 1 ? "" : "s"} to keep or drop in the Diff tab`}
          {unsent > 0 && ` · ${unsent} note${unsent === 1 ? "" : "s"} not sent to an agent`}
        </span>
        <div className="spacer" />
        <ReviewerButton taskId={taskId} />
      </div>
      {going.map((r) => (
        <div key={r.checkout_id} className="outgoing-repo">
          <div className="muted">
            <code>{r.repo}</code>{" "}
            {r.error
              ? <span className="del">could not be read: {r.error}</span>
              : `${r.commits.length}${r.commits.length >= 100 ? "+" : ""} commit${r.commits.length === 1 ? "" : "s"} going out: ` +
                r.commits.slice(0, 3).map((c) => c.subject).join("; ") +
                (r.commits.length > 3 ? "; …" : "")}
          </div>
          {r.leftovers.map((l, i) => (
            <div key={i} className="outgoing-leftover" title={l.text}>
              <span className={`chip ${KIND_TONE[l.kind]}`}>{l.kind}</span>
              <code>{l.path}:{l.line}</code>
              <span className="text">{l.text}</span>
            </div>
          ))}
          {r.leftovers_more && <div className="muted">and more not listed</div>}
        </div>
      ))}
    </div>
  );
}
