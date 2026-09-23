import {
  isPermissionGranted,
  requestPermission,
} from "@tauri-apps/plugin-notification";
import { api } from "./api";
import type { NotifyTarget, ReviewQueue, ReviewRequest } from "./types";

/** Past this, one banner rather than a stack of them. */
export const NOTIFY_BATCH = 3;

export function reviewIdentity(pr: Pick<ReviewRequest, "repo" | "number">): string {
  return pr.repo ? `${pr.repo}#${pr.number}` : `#${pr.number}`;
}

/**
 * What to remember after this snapshot.
 *
 * A team lookup that failed comes back with no pull requests. Forgetting the
 * ones we already knew would make the next success announce all of them.
 */
export function nextReviewKeys(queue: ReviewQueue, prev: Set<string> | null): Set<string> {
  const next = new Set(queue.mine.map(reviewIdentity));
  if (queue.team && !queue.team.error) {
    for (const pr of queue.team.prs) next.add(reviewIdentity(pr));
  } else if (queue.team?.error && prev) {
    for (const key of prev) next.add(key);
  }
  return next;
}

/** Pull requests in `next` that `prev` had not seen. `prev` null is the first snapshot. */
export function unseenReviews(
  queue: ReviewQueue,
  prev: Set<string> | null,
): ReviewRequest[] {
  if (!prev) return [];
  const all = [...queue.mine];
  if (queue.team && !queue.team.error) all.push(...queue.team.prs);
  return all.filter((pr) => !prev.has(reviewIdentity(pr)));
}

let asked = false;
let granted = false;

/** Ask once per launch. A refusal stays a refusal until the app is opened again. */
async function allowed(): Promise<boolean> {
  if (granted) return true;
  if (await isPermissionGranted()) {
    granted = true;
    return true;
  }
  if (asked) return false;
  asked = true;
  granted = (await requestPermission()) === "granted";
  return granted;
}

export async function announceReviews(fresh: ReviewRequest[]): Promise<void> {
  if (fresh.length > NOTIFY_BATCH) {
    await systemBanner(
      "Reviews",
      `${fresh.length} pull requests are waiting on a review`,
      "reviews",
    );
    return;
  }
  for (const pr of fresh) {
    await systemBanner("Review requested", `${reviewIdentity(pr)} — ${pr.title}`, "reviews");
  }
}

export async function announceTickets(
  fresh: { key: string; summary: string }[],
): Promise<void> {
  if (fresh.length > NOTIFY_BATCH) {
    await systemBanner("Tickets", `${fresh.length} tickets were assigned to you`, "tickets");
    return;
  }
  for (const issue of fresh) {
    await systemBanner("New ticket", `${issue.key} — ${issue.summary}`, "tickets");
  }
}

/** Ask for permission while the window is open, so a later banner is not the first time. */
export function prepareNotifications(): Promise<boolean> {
  return allowed();
}

export async function systemBanner(
  title: string,
  body: string,
  target: NotifyTarget,
): Promise<void> {
  if (!(await allowed())) return;
  await api.systemNotify(title, body, target);
}
