/**
 * GitHub's side of the Rust types mirrored in `types.ts`: pull requests,
 * their checks and reviews, the feedback sent to agents, and the review
 * queue. Split from `types.ts` at its size ceiling; import from there.
 */
import type { TicketMove } from "./types";

export interface PullRequest {
  number: number;
  title: string;
  state: string;
  draft: boolean;
  author: string;
  head: string;
  /** The commit the branch was at on GitHub; for a merged PR, what landed. */
  head_sha: string;
  base: string;
  url: string;
  mergeable_state: string | null;
  merged: boolean;
  /** Conversation comments and inline review comments. */
  comments: number;
  review_comments: number;
}

export interface Review {
  author: string;
  /** APPROVED, CHANGES_REQUESTED, COMMENTED or DISMISSED. */
  state: string;
  submitted_at: string | null;
  url: string;
}

/** What a PR's reviews add up to. */
export type Verdict = "approved" | "changes_requested" | "commented" | "none";

export interface CheckRun {
  name: string;
  status: string;
  conclusion: string | null;
  url: string | null;
}

export interface CheckoutPr {
  checkout_id: string;
  repo: string;
  pr: PullRequest | null;
  checks: CheckRun[];
  reviews: Review[];
  /** Earlier pull requests from this branch, newest first. */
  past: PullRequest[];
  verdict: Verdict;
  /** Where this repo's next PR is opened against; may differ from pr.base. */
  base: string;
  /** Files changed on the branch — or, once its PR has merged, since what it landed. */
  changed: number;
  error: string | null;
}

export interface RepoBranchFacts {
  checkout_id: string;
  repo: string;
  base: string;
  /** Abbreviated commit the branch is measured from. */
  baseline: string;
  /** True when that is the branch point recorded at creation, not a merge base. */
  baseline_recorded: boolean;
  commits: number;
  unpushed: number;
  has_remote: boolean;
}

/** One comment, review or reply on a pull request. */
export interface FeedbackNote {
  author: string;
  /** Written by an app — a coverage bot, a linter — not a person. */
  bot: boolean;
  /** For a review: APPROVED, CHANGES_REQUESTED or COMMENTED. */
  state: string | null;
  body: string;
  url: string;
  at: string | null;
}

/** A line of the code under a review thread; `n` is null on the other side. */
export interface CodeLine {
  n: number | null;
  op: "+" | "-" | " ";
  text: string;
}

export interface ReviewThread {
  path: string;
  line: number | null;
  /** Where a comment on a range of lines begins. */
  start_line: number | null;
  code: CodeLine[];
  resolved: boolean;
  /** The code under it has changed since. */
  outdated: boolean;
  url: string;
  comments: FeedbackNote[];
}

export interface FailedCheck {
  name: string;
  conclusion: string;
  url: string | null;
  summary: string;
  /** The end of the job's log, for a GitHub Actions job. */
  log: string | null;
}

/** What reviewers and CI have said on one repository's open PR. */
export interface RepoFeedback {
  checkout_id: string;
  repo: string;
  number: number;
  title: string;
  url: string;
  /** Who opened the PR. */
  author: string;
  threads: ReviewThread[];
  reviews: FeedbackNote[];
  comments: FeedbackNote[];
  checks: FailedCheck[];
  error: string | null;
}

/** A piece of feedback chosen to go to an agent. */
export type FeedbackItem =
  | {
      kind: "thread"; checkout_id: string; path: string; line: number | null;
      start_line: number | null; code: CodeLine[]; outdated: boolean; resolved: boolean; url: string; comments: { author: string; body: string }[];
    }
  | { kind: "review"; checkout_id: string; author: string; state: string | null; body: string; url: string }
  | { kind: "comment"; checkout_id: string; author: string; body: string; url: string }
  | {
      kind: "check"; checkout_id: string; name: string; conclusion: string;
      url: string | null; summary: string; log: string | null;
    };

export interface TaskPrs {
  task_id: string;
  rows: CheckoutPr[];
  ticket: TicketMove | null;
}

/** A pull request waiting on a person or a team. */
export interface ReviewRequest {
  repo: string;
  number: number;
  title: string;
  url: string;
  author: string;
  draft: boolean;
  updated_at: string;
}

export interface TeamReviews {
  /** `org/slug`. */
  slug: string;
  name: string;
  prs: ReviewRequest[];
  more: boolean;
  error: string | null;
}

export interface ReviewQueue {
  mine: ReviewRequest[];
  mine_more: boolean;
  /** Null when no review team is configured. */
  team: TeamReviews | null;
  /** The pull requests you opened (REV-4). Always sent to the view. */
  authored: AuthoredPrs | null;
}

export interface AuthoredPrs {
  prs: AuthoredPr[];
  more: boolean;
  error: string | null;
}

export interface AuthoredPr {
  /** `owner/name`. */
  repo: string;
  number: number;
  title: string;
  url: string;
  draft: boolean;
  updated_at: string;
  /** Its branch, and the one it merges into. */
  head: string;
  base: string;
  /** As GitHub rolls up check runs and commit statuses; "none" when nothing reports. */
  checks: "passing" | "failing" | "pending" | "none";
  /** Branch protection's decision, or the latest reviews where it asks for none. */
  review: "approved" | "changes_requested" | "review_required" | "none";
  approved_by: string[];
  changes_by: string[];
  /** Asked and not yet reviewed: logins, and teams as `@slug`. */
  waiting_on: string[];
  unresolved: number;
  /** More threads than were counted: `unresolved` is a floor. */
  unresolved_more: boolean;
  conflicts: boolean;
}
