import { useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api } from "../lib/api";
import { reportRepoResults } from "../lib/report";
import { useStore, type ReviewNote } from "../store";
import type { ReviewRequest, TaskView } from "../lib/types";
import { DiffView } from "./DiffView";
import { ReviewHeader } from "./ReviewHeader";
import { Confirm } from "./ui";

const NO_NOTES: ReviewNote[] = [];

/** What a review is called on its page: the PR's title, without the task's prefix. */
function titleOf(task: TaskView): string {
  return task.name.replace(/^Review: /, "");
}

/** The pull request in "To review", where the queue still lists it. */
function useQueued(task: TaskView): ReviewRequest | null {
  return useStore((s) => {
    const r = task.review;
    if (!r) return null;
    const repo = r.repo.toLowerCase();
    const q = s.reviewQueue;
    return [...(q?.mine ?? []), ...(q?.team?.prs ?? [])].find((p) => p.repo.toLowerCase() === repo && p.number === r.number) ?? null;
  });
}

/**
 * One pull request under review (REV-10), in the Reviews view and nowhere
 * else: a review is not your work, and shown as a task it sat among your
 * tasks with a Commit button and a "never pushed" that meant nothing. The
 * three steps are on the page, since nothing else says what to do here.
 */
export function ReviewPage({ task }: { task: TaskView }) {
  const review = task.review!;
  const open = useStore((s) => s.openReviewTask);
  const cursorIde = useStore((s) => s.cursorIde);
  const fail = useStore((s) => s.fail);
  const toast = useStore((s) => s.toast);
  const refreshTasks = useStore((s) => s.refreshTasks);
  const dropNotes = useStore((s) => s.dropNotes);
  const notes = useStore((s) => s.notes[task.id] ?? NO_NOTES);
  const running = useStore((s) => s.reviewer[task.id]?.running ?? false);
  const pr = useQueued(task);
  const [finishing, setFinishing] = useState(false);
  const ready = notes.filter((n) => n.by === "you" || n.kept).length;
  const undecided = notes.length - ready;

  async function finish() {
    try {
      reportRepoResults(toast, await api.deleteTask(task.id, true), "Removed");
      dropNotes(task.id, notes.map((n) => n.id));
      open(null);
      await refreshTasks();
    } catch (e) {
      fail(e);
    }
  }

  return (
    <div className="main review-page">
      <div className="ws-header">
        <button className="btn btn-sm" onClick={() => open(null)} title="Back to the pull requests waiting on you">
          ← Reviews
        </button>
        <h1 title={titleOf(task)}>{titleOf(task)}</h1>
        <span className="muted nowrap">
          {review.repo}#{review.number}{review.author && ` by ${review.author}`}
          {pr && ` · +${pr.additions} −${pr.deletions}`}
        </span>
        <div className="spacer" />
        <div className="chips">
          <ReviewHeader taskId={task.id} review={review} />
          {cursorIde && (
            <button className="btn btn-sm" title={`Open ${task.root} in Cursor`} onClick={() => void api.openInCursor(task.root).catch(fail)}>
              Cursor ↗
            </button>
          )}
          {task.issue_url && (
            <button className="btn btn-sm" onClick={() => void openUrl(task.issue_url!)}>{task.issue_key} ↗</button>
          )}
          <button className="btn btn-sm" title="Done with this review: remove its checkout" onClick={() => setFinishing(true)}>
            Finish
          </button>
        </div>
      </div>

      <ol className="review-steps">
        <li>Read the changes. Click a line number to leave a comment.</li>
        <li className={running ? "active" : ""}>
          Optionally, <b>Review with Claude</b> for a first pass: keep or drop what it finds.
          {undecided > 0 && (
            <button
              className="chip warn"
              title="Go to the next finding"
              onClick={() => useStore.getState().showNextFinding(task.id)}
            >
              {undecided} to decide →
            </button>
          )}
        </li>
        <li className={ready > 0 ? "active" : ""}>
          <b>Post review…</b> sends your comments to GitHub as one review, after showing you all of it.
          {ready > 0 && <span className="chip">{ready} ready</span>}
        </li>
      </ol>

      <div className="content">
        <DiffView task={task} />
      </div>

      {finishing && (
        <Confirm
          title="Finish this review?"
          body={
            ready + undecided > 0
              ? `Its checkout goes, and with it ${ready + undecided} note${ready + undecided === 1 ? "" : "s"} not posted.`
              : "Its checkout goes. What you posted stays on GitHub."
          }
          confirmLabel="Finish"
          busyLabel="Removing…"
          onConfirm={finish}
          onCancel={() => setFinishing(false)}
        />
      )}
    </div>
  );
}

/** A review you have started (REV-10), listed even once the PR has left your queue. */
export function ReviewInProgress({ task }: { task: TaskView }) {
  const open = useStore((s) => s.openReviewTask);
  const notes = useStore((s) => s.notes[task.id]?.length ?? 0);
  const pr = useQueued(task);
  const review = task.review!;
  return (
    <button type="button" className="review-card" onClick={() => open(task.id)} title="Continue this review">
      <div className="top">
        <span className="repo">{review.repo}</span>
        <span className="num">#{review.number}</span>
        <span className="spacer" />
        <span className="chip">continue →</span>
      </div>
      <div className="title">{titleOf(task)}</div>
      <div className="chips standing">
        {review.author && <span className="by">{review.author}</span>}
        {notes > 0 && <span className="chip warn">{notes} note{notes === 1 ? "" : "s"} not posted</span>}
        {pr ? (
          pr.head_sha !== review.head_sha && <span className="chip warn">pushed to since</span>
        ) : (
          <span className="chip" title="GitHub no longer lists it as waiting on you or your team">no longer waiting on you</span>
        )}
      </div>
    </button>
  );
}
