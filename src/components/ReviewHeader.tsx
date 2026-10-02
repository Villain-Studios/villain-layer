import { useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api } from "../lib/api";
import { useStore } from "../store";
import type { ReviewOf } from "../lib/types";
import { Spinner } from "./ui";

/**
 * A review task's place in its header (REV-10): whose pull request it is,
 * and taking the author's latest push. It stands where "Update from base"
 * stands for your own work, since a review follows its pull request, not
 * the base.
 */
export function ReviewHeader({ taskId, review }: { taskId: string; review: ReviewOf }) {
  const fail = useStore((s) => s.fail);
  const toast = useStore((s) => s.toast);
  const refreshTasks = useStore((s) => s.refreshTasks);
  // "To review" knows the pull request's head now; the task knows the one
  // it was last moved to. Either list may hold it.
  const head = useStore((s) => {
    const q = s.reviewQueue;
    const repo = review.repo.toLowerCase();
    const pr = [...(q?.mine ?? []), ...(q?.team?.prs ?? [])].find(
      (p) => p.repo.toLowerCase() === repo && p.number === review.number,
    );
    return pr?.head_sha ?? null;
  });
  const behind = head !== null && head !== review.head_sha;
  const [taking, setTaking] = useState(false);

  async function takeLatest() {
    setTaking(true);
    try {
      const moved = await api.reviewTakeLatest(taskId);
      await refreshTasks();
      toast(
        "success",
        moved.review?.head_sha === review.head_sha ? "Already at its latest push" : "Moved to its latest push",
      );
    } catch (e) {
      fail(e);
    } finally {
      setTaking(false);
    }
  }

  return (
    <>
      <span className="chip task" title={`At ${review.head_sha.slice(0, 10)}`}>
        reviewing {review.repo}#{review.number}{review.author && ` by ${review.author}`}
      </span>
      {behind && <span className="chip warn">pushed to since</span>}
      <button
        className="btn btn-sm"
        disabled={taking}
        title="Fetch the pull request's head and move this review to it. Refused while you have edits here."
        onClick={() => void takeLatest()}
      >
        {taking ? <Spinner /> : "Take latest"}
      </button>
      <button className="btn btn-sm" title={review.url} onClick={() => void openUrl(review.url).catch(fail)}>
        GitHub ↗
      </button>
    </>
  );
}
