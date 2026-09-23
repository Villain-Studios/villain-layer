import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api } from "../lib/api";
import { copyText } from "../lib/clipboard";
import { reportRepoResults } from "../lib/report";
import { reviewComments, taskReview, useStore, type TaskReview } from "../store";
import type { CheckoutPr, CheckRun, Review, TaskView } from "../lib/types";
import { Combo, Field, Modal, Spinner } from "./ui";
import { PrFeedback } from "./PrFeedback";
import { FinishTask } from "./FinishTask";

/**
 * The latest review from each reviewer, which is the one GitHub itself shows.
 *
 * Reviewers come back and change their minds; listing every submission would
 * show a PR as both approved and blocked by the same person.
 */
function latestByAuthor(reviews: Review[]): Review[] {
  const by = new Map<string, Review>();
  for (const r of reviews) {
    // A comment after a decision does not undo it — the rule the verdict on
    // the card follows. Letting it through showed a reviewer who approved and
    // then replied as having only commented, under an "approved" chip.
    const prev = by.get(r.author);
    if ((r.state === "COMMENTED" || r.state === "PENDING") && prev && prev.state !== "COMMENTED") {
      continue;
    }
    by.set(r.author, r);
  }
  return [...by.values()];
}

function reviewColor(state: string) {
  if (state === "APPROVED") return "var(--green)";
  if (state === "CHANGES_REQUESTED") return "var(--red)";
  return "var(--dim)";
}

const REVIEW_WORDS: Record<string, string> = {
  APPROVED: "approved",
  CHANGES_REQUESTED: "requested changes",
  COMMENTED: "commented",
  DISMISSED: "dismissed",
};

/** Where the task as a whole stands, in one line. */
const TASK_REVIEW: Record<TaskReview, { word: string; color: string }> = {
  none: { word: "", color: "var(--dim)" },
  incomplete: { word: "Partly up for review", color: "var(--amber)" },
  open: { word: "In review", color: "var(--blue)" },
  commented: { word: "In review, with comments", color: "var(--blue)" },
  changes_requested: { word: "Changes requested", color: "var(--red)" },
  approved: { word: "Approved", color: "var(--green)" },
  merged: { word: "Merged", color: "var(--green)" },
};

/**
 * One row per check name, keeping the worst outcome.
 *
 * A commit can carry several check suites — the same workflow run once for the
 * push and again for the pull request — so the same names come back twice and
 * the list doubles. Collapsing on the worst means a failure can never hide
 * behind a duplicate of itself that happened to pass.
 */
function worstByName(checks: CheckRun[]): CheckRun[] {
  const rank = (c: CheckRun) => {
    if (c.status !== "completed") return 1;
    if (c.conclusion === "success" || c.conclusion === "skipped" || c.conclusion === "neutral") {
      return 0;
    }
    return 2;
  };
  const by = new Map<string, CheckRun>();
  for (const c of checks) {
    const seen = by.get(c.name);
    if (!seen || rank(c) > rank(seen)) by.set(c.name, c);
  }
  return [...by.values()];
}

const NO_ROWS: CheckoutPr[] = [];

/** A finished run that went wrong. Cancelled is left out: that is usually a newer push. */
function failed(c: CheckRun) {
  return (
    c.status === "completed" &&
    !["success", "skipped", "neutral", "cancelled"].includes(c.conclusion ?? "")
  );
}

function checkColor(c: CheckRun) {
  if (c.status !== "completed") return "var(--amber)";
  if (c.conclusion === "success") return "var(--green)";
  if (c.conclusion === "skipped" || c.conclusion === "neutral") return "var(--dim)";
  return "var(--red)";
}

export function PrPanel({
  task, onUpdateFromBase,
}: {
  task: TaskView;
  onUpdateFromBase: () => void;
}) {
  const settings = useStore((s) => s.settings);
  const allPanes = useStore((s) => s.panes);
  const agentPanes = useMemo(
    () => allPanes.filter((p) => p.task_id === task.id && p.kind === "agent" && p.running),
    [allPanes, task.id],
  );
  const toggleSettings = useStore((s) => s.toggleSettings);
  const toast = useStore((s) => s.toast);
  const fail = useStore((s) => s.fail);

  /**
   * The rows, held in the store rather than here.
   *
   * The background watch writes them for every task on a timer and this panel
   * fetches them for one on demand. Two copies meant every action had to
   * refresh both and one of them always forgot; with a single copy, whoever
   * fetched last is simply what everyone sees.
   */
  const rows = useStore((s) => s.prs[task.id]) ?? NO_ROWS;
  const setTaskPrs = useStore((s) => s.setTaskPrs);
  /**
   * Whether GitHub has answered yet.
   *
   * Without this the panel renders its empty state first — the form for
   * opening a PR — and then replaces it with the PR that was there all along.
   * An answer that arrives a moment later is not a reason to show the wrong
   * one in the meantime.
   */
  const [askedFor, setAskedFor] = useState<string | null>(null);
  // Per task, because the panel is not remounted when you switch between them:
  // a flag left true from the last one would show this one's empty state while
  // its own rows were still in flight.
  const loaded = askedFor === task.id || rows.length > 0;
  const [branches, setBranches] = useState<Record<string, string[]>>({});
  /** Whether the "open a pull request" dialog is up. */
  const [creating, setCreating] = useState(false);
  const [feedback, setFeedback] = useState(false);
  const [finishing, setFinishing] = useState(false);
  /** Which PR cards are expanded, when there are enough to be worth folding. */
  const [cards, setCards] = useState<Record<string, boolean>>({});
  const [showPast, setShowPast] = useState(false);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState(false);
  const [title, setTitle] = useState(task.name);
  const [body, setBody] = useState("");
  const [draft, setDraft] = useState(true);
  const [drafting, setDrafting] = useState(false);
  const pollRef = useRef<number | null>(null);

  const load = useCallback(async () => {
    if (!settings?.github_connected) return;
    setLoading(true);
    try {
      setTaskPrs(task.id, await api.githubTaskPrs(task.id));
    } catch (e) {
      fail(e);
    } finally {
      // An error is an answer too: never setting this would hide the form for
      // good and offer nothing in its place.
      setAskedFor(task.id);
      setLoading(false);
    }
  }, [task.id, settings?.github_connected, fail]);

  useEffect(() => { void load(); }, [load]);

  // What the base field offers. Keyed on the set of repos rather than the rows
  // themselves, so a sweep landing every ninety seconds does not refetch it.
  const checkoutIds = rows.map((r) => r.checkout_id).join(",");
  useEffect(() => {
    if (!creating) return;
    let stop = false;
    void (async () => {
      const ids = checkoutIds.split(",").filter(Boolean);
      // Independent of each other, so they are not waited for in turn.
      const lists = await Promise.all(
        ids.map((id) =>
          // A repo whose branches cannot be listed still takes a typed base.
          api.checkoutBranches(id).catch(() => [] as string[]),
        ),
      );
      if (stop) return;
      setBranches(Object.fromEntries(ids.map((id, i) => [id, lists[i]])));
    })();
    return () => { stop = true; };
  }, [checkoutIds, creating]);

  // Stop waiting for a draft if the panel goes away.
  useEffect(() => () => { if (pollRef.current) window.clearInterval(pollRef.current); }, []);

  // The description arrives a few words at a time. Showing it as it is written
  // is most of what makes this feel quick: the wait is the same, but it starts
  // reading like an answer straight away instead of a spinner.
  //
  // Only while a draft is being written: a chunk still in flight when the
  // finished text landed was appended to it, repeating the end.
  const streaming = useRef(false);
  useEffect(() => {
    const p = listen<{ task_id: string; text: string }>("pr:draft", (e) => {
      if (e.payload.task_id !== task.id || !streaming.current) return;
      setBody((current) => current + e.payload.text);
    });
    return () => { void p.then((un) => un()); };
  }, [task.id]);

  useEffect(() => {
    setTitle(task.name);
    setBody(task.issue_url ? `Jira: ${task.issue_url}\n` : "");
  }, [task.id, task.name, task.issue_url]);

  function openPr(url: string) {
    void openUrl(url).catch(() => toast("error", "Could not open the browser"));
  }

  function copyPr(url: string, number: number) {
    void copyText(url)
      .then(() => toast("success", `Copied the link to #${number}`))
      .catch(() => toast("error", "Could not reach the clipboard"));
  }

  /** Where this repo's next PR goes. Changing it leaves an open PR alone. */
  async function setBase(row: CheckoutPr, base: string) {
    if (base.trim() === row.base || !base.trim()) return;
    try {
      await api.setCheckoutBase(row.checkout_id, base);
      // The task list carries the base too, and Update from base reads it
      // from there: left alone it said "← origin/main" while the backend
      // merged the new one.
      await Promise.all([load(), useStore.getState().refreshTasks().catch(() => {})]);
    } catch (e) {
      fail(e);
    }
  }

  async function retarget(row: CheckoutPr) {
    try {
      toast("success", await api.githubRetargetPr(row.checkout_id));
      await load();
    } catch (e) {
      fail(e);
    }
  }

  /// Draft the description from the diff, with a cheap one-shot model.
  ///
  /// Fast because it asks nothing of the working agent: no shared context to
  /// re-read, no waiting for it to finish its turn, no file to hand back. If
  /// that is not possible, fall back to asking the agent itself — slower, but
  /// it knows what the diff cannot say.
  async function draftWithAgent() {
    setDrafting(true);
    // Cleared so the streamed text is not appended to whatever was there.
    setBody("");
    streaming.current = true;
    try {
      const drafted = await api.draftPrDescription(task.id);
      streaming.current = false;
      setBody(drafted.trim());
      toast("success", "Description drafted — edit it before opening the PR.");
      setDrafting(false);
      return;
    } catch (e) {
      // Whatever was streamed before it failed is half an answer, not an answer.
      streaming.current = false;
      setBody("");
      const pane = agentPanes[0];
      if (!pane) {
        setDrafting(false);
        fail(e);
        return;
      }
      toast("info", `Asking ${pane.title} instead — this one takes longer.`);
    }

    const pane = agentPanes[0];
    try {
      await api.requestPrDescription(task.id, pane.id);

      const started = Date.now();
      if (pollRef.current) window.clearInterval(pollRef.current);
      pollRef.current = window.setInterval(async () => {
        const text = await api.takePrDescription(task.id).catch(() => null);
        if (text) {
          setBody(text.trim());
          setDrafting(false);
          if (pollRef.current) window.clearInterval(pollRef.current);
          toast("success", "Description drafted — edit it before opening the PR.");
        } else if (Date.now() - started > 5 * 60 * 1000) {
          setDrafting(false);
          if (pollRef.current) window.clearInterval(pollRef.current);
          toast("error", "Gave up waiting for the draft. The agent may still be working.");
        }
      }, 2000);
    } catch (e) {
      setDrafting(false);
      fail(e);
    }
  }

  async function push() {
    setBusy(true);
    try {
      reportRepoResults(toast, await api.pushTask(task.id), "Pushed");
      await load();
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  async function openPrs() {
    setBusy(true);
    try {
      reportRepoResults(toast, await api.githubOpenPrs(task.id, title.trim(), body, draft), "Opened");
      setCreating(false);
      await load();
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  if (!settings?.github_connected) {
    return (
      <div className="empty">
        <h2>GitHub not connected</h2>
        <p>Connect GitHub or GitHub Enterprise to open pull requests and read CI from here.</p>
        <button className="btn" onClick={() => toggleSettings(true)}>Open settings</button>
      </div>
    );
  }

  // A PR that has merged or closed no longer covers the repo: work committed
  // after it landed still needs one, and the row keeps the old PR only so the
  // panel can show what became of it.
  const pending = rows.filter((r) => (!r.pr || r.pr.state !== "open") && r.changed > 0);
  const review = taskReview(rows);
  const said = reviewComments(rows);

  // A card is only drawn for a repository's current pull request, because that
  // is the only one whose reviews and checks were fetched. Everything else the
  // branch has been through is a line in the list below — kept, so that opening
  // a second attempt does not erase the first from the record.
  const live = rows.filter((r) => r.pr && r.pr.state === "open");
  const earlier = rows.flatMap((r) => [
    ...(r.pr && r.pr.state !== "open" ? [{ row: r, pr: r.pr }] : []),
    ...r.past.map((pr) => ({ row: r, pr })),
  ]);
  const entries = live.length + earlier.length;
  // What there is to hand an agent: counts from the sweep, so the button can
  // say so before anything is fetched.
  const failing = live.reduce((n, r) => n + worstByName(r.checks).filter(failed).length, 0);

  const repoRows = (
    <div className="muted" style={{ marginBottom: 10, lineHeight: 1.6 }}>
      {rows.map((r) => (
        <div key={r.checkout_id} className="row">
          <span
            className="dot"
            style={{
              background:
                r.pr && r.pr.state === "open"
                  ? "var(--green)"
                  : r.changed > 0
                    ? "var(--amber)"
                    : "var(--dimmer)",
            }}
          />
          <span style={{ fontFamily: "var(--mono)", fontSize: 11.5, width: 140 }}>{r.repo}</span>
          <span>
            {r.pr && r.pr.state === "open"
              ? `PR #${r.pr.number} open`
              : r.changed > 0
                ? `${r.changed} changed file${r.changed === 1 ? "" : "s"}`
                : "no changes"}
          </span>
          <div className="spacer" />
          <span>→</span>
          {/*
            Typing still works: the list is the worktree's remote-tracking
            refs, so a branch pushed since the last fetch is not in it.
          */}
          <Combo
            value={r.base}
            options={branches[r.checkout_id] ?? []}
            width={240}
            title="The branch this repository's pull request is opened against"
            empty="No branch matches"
            onChange={(v) => void setBase(r, v)}
          />
        </div>
      ))}
    </div>
  );

  return (
    <div className="panel-scroll">
      <div className="row" style={{ marginBottom: 12 }}>
        {review !== "none" ? (
          <>
            <span className="dot" style={{ background: TASK_REVIEW[review].color }} />
            <b>{TASK_REVIEW[review].word}</b>
            <span className="muted">
              {live.length} PR{live.length === 1 ? "" : "s"}
              {/* A task is only as reviewed as its least reviewed repository. */}
              {review === "incomplete" &&
                `, ${pending.length} repo${pending.length === 1 ? "" : "s"} still without one`}
              {said > 0 && ` · ${said} comment${said === 1 ? "" : "s"}`}
            </span>
          </>
        ) : (
          <b>Pull requests</b>
        )}
        <div className="spacer" />
        {loading && <Spinner />}
        {live.length > 0 && (
          <button
            className="btn btn-sm"
            title="Pick review threads, comments and failing checks to hand an agent"
            onClick={() => setFeedback(true)}
          >
            Feedback → agent
            {said + failing > 0 && <span className="badge">{said + failing}</span>}
          </button>
        )}
        <button className="btn btn-sm" onClick={() => void load()}>Refresh</button>
        <button className="btn btn-sm" disabled={busy} onClick={() => void push()}>
          Push all
        </button>
        {entries > 0 && (
          <button
            className="btn btn-sm btn-primary"
            disabled={pending.length === 0}
            title={
              pending.length === 0
                ? "Every repository with changes already has an open pull request"
                : `Open a pull request in ${pending.length} repositor${
                    pending.length === 1 ? "y" : "ies"
                  }`
            }
            onClick={() => setCreating(true)}
          >
            +
          </button>
        )}
      </div>

      {review === "merged" && (
        <div className="card row">
          <span className="dot" style={{ background: "var(--green)" }} />
          <span className="muted" style={{ flex: 1, lineHeight: 1.5 }}>
            Every pull request has merged
            {pending.length > 0
              ? `, but ${pending.map((r) => r.repo).join(", ")} ${
                  pending.length === 1 ? "has" : "have"
                } work since. Finishing keeps ${pending.length === 1 ? "that branch" : "those branches"}.`
              : "."}
          </span>
          <button className="btn btn-sm btn-primary" onClick={() => setFinishing(true)}>
            Finish task…
          </button>
        </div>
      )}

      {loaded && entries === 0 && (
        <div className="card">
          <div className="muted" style={{ lineHeight: 1.6, marginBottom: 12 }}>
            Nothing has been opened from <code>{task.branch}</code> yet.
            {pending.length > 0
              ? ` ${pending.length} repositor${pending.length === 1 ? "y has" : "ies have"} changes ready.`
              : " No repository has changes to open one with."}
          </div>
          <button
            className="btn btn-primary"
            disabled={pending.length === 0}
            onClick={() => setCreating(true)}
          >
            Open pull request
          </button>
        </div>
      )}

      {live.map((row) => {
        const pr = row.pr!;
        const key = row.checkout_id;
        // Several fold, so the list reads as a list rather than as a wall of
        // check runs. One on its own is always shown — and stays shown if the
        // others go away while it is folded, which would otherwise leave it
        // collapsed with no chevron left to open it.
        const foldable = live.length > 1;
        const shown = !foldable || (cards[key] ?? false);
        return (
          <div key={key} className="card">
            <div
              className="row"
              style={foldable ? { cursor: "pointer" } : undefined}
              onClick={foldable ? () => setCards((c) => ({ ...c, [key]: !shown })) : undefined}
            >
              {foldable && <span className={`chev${shown ? " open" : ""}`}>▶</span>}
              <h3 style={{ margin: 0 }}>
                <span style={{ color: "var(--dim)" }}>{row.repo}</span> #{pr.number} {pr.title}
              </h3>
              <div className="spacer" />
              {pr.draft && <span className="chip">draft</span>}
              {/* GitHub's own reading of the branch against its base. */}
              {(pr.mergeable_state === "dirty" || pr.mergeable_state === "behind") && (
                <button
                  className={`chip ${pr.mergeable_state === "dirty" ? "del" : "warn"}`}
                  title={`Bring ${pr.base} into this branch — merge or rebase`}
                  onClick={(e) => { e.stopPropagation(); onUpdateFromBase(); }}
                >
                  {pr.mergeable_state === "dirty" ? `conflicts with ${pr.base}` : `behind ${pr.base}`}
                </button>
              )}
              {row.verdict === "approved" && <span className="chip add">approved</span>}
              {row.verdict === "changes_requested" && (
                <span className="chip del">changes requested</span>
              )}
              {/* Stopped, or opening the PR would fold the card underneath it. */}
              <button
                className="btn-sm"
                title="Open on GitHub"
                onClick={(e) => { e.stopPropagation(); openPr(pr.url); }}
              >
                ↗
              </button>
              <button
                className="btn-sm"
                title="Copy link"
                onClick={(e) => { e.stopPropagation(); copyPr(pr.url, pr.number); }}
              >
                ⧉
              </button>
            </div>

            {shown && (
              <>
                <div className="muted" style={{ marginTop: 6 }}>
                  {pr.head} → {pr.base} · opened by {pr.author}
                  {pr.comments + pr.review_comments > 0 && (
                    <>
                      {" · "}
                      {pr.comments + pr.review_comments} comment
                      {pr.comments + pr.review_comments === 1 ? "" : "s"}
                    </>
                  )}
                </div>

                {pr.base !== row.base && (
                  <div className="row" style={{ marginTop: 10 }}>
                    <span className="chip warn">targets {pr.base}</span>
                    <button className="btn btn-sm" onClick={() => void retarget(row)}>
                      Move onto {row.base}
                    </button>
                  </div>
                )}

                {row.reviews.length > 0 && (
                  <div style={{ marginTop: 10 }}>
                    {latestByAuthor(row.reviews).map((r) => (
                      <div key={r.author} className="check">
                        <span className="dot" style={{ background: reviewColor(r.state) }} />
                        <span className="name">{r.author}</span>
                        <span className="muted">
                          {REVIEW_WORDS[r.state] ?? r.state.toLowerCase()}
                        </span>
                        {r.url && (
                          <button className="btn-sm" onClick={() => openPr(r.url)}>↗</button>
                        )}
                      </div>
                    ))}
                  </div>
                )}

                {row.checks.length > 0 && (
                  <div style={{ marginTop: 10 }}>
                    {worstByName(row.checks).map((c, i) => (
                      <div key={`${c.name}:${i}`} className="check">
                        <span className="dot" style={{ background: checkColor(c) }} />
                        <span className="name">{c.name}</span>
                        <span className="muted">{c.conclusion ?? c.status}</span>
                        {c.url && (
                          <button className="btn-sm" onClick={() => openPr(c.url!)}>↗</button>
                        )}
                      </div>
                    ))}
                  </div>
                )}
              </>
            )}
          </div>
        );
      })}

      {earlier.length > 0 && (
        <div className="card">
          <div
            className="row"
            style={{ cursor: "pointer" }}
            onClick={() => setShowPast((v) => !v)}
          >
            <span className={`chev${showPast ? " open" : ""}`}>▶</span>
            <h3 style={{ margin: 0 }}>Earlier pull requests</h3>
            <span className="muted">{earlier.length}</span>
          </div>
          {showPast &&
            earlier.map(({ row, pr }) => (
              <div key={`${row.checkout_id}:${pr.number}`} className="check">
                <span
                  className="dot"
                  style={{ background: pr.merged ? "var(--green)" : "var(--dim)" }}
                />
                <span className="name">
                  {row.repo} #{pr.number}
                </span>
                <span className="muted">
                  {pr.merged
                    ? `merged into ${pr.base}`
                    : pr.state === "open"
                      ? `still open against ${pr.base}`
                      : "closed without merging"}
                </span>
                <button className="btn-sm" title="Open on GitHub" onClick={() => openPr(pr.url)}>
                  ↗
                </button>
                <button
                  className="btn-sm"
                  title="Copy link"
                  onClick={() => copyPr(pr.url, pr.number)}
                >
                  ⧉
                </button>
              </div>
            ))}
        </div>
      )}

      {loaded && rows.some((r) => r.error) && (
        <div className="card">
          <h3>Repos that could not be read</h3>
          {rows.filter((r) => r.error).map((r) => (
            <div key={r.checkout_id} className="check">
              <span className="dot" style={{ background: "var(--red)" }} />
              <span className="name">{r.repo}</span>
              <span className="muted">{r.error}</span>
            </div>
          ))}
        </div>
      )}

      {feedback && <PrFeedback task={task} onClose={() => setFeedback(false)} />}
      {finishing && <FinishTask task={task} onClose={() => setFinishing(false)} />}

      {creating && (
        <Modal
          title="Open pull request"
          wide
          onClose={() => setCreating(false)}
          footer={
            <>
              <button className="btn" onClick={() => setCreating(false)}>Cancel</button>
              <button
                className="btn btn-primary"
                disabled={busy || pending.length === 0 || !title.trim()}
                onClick={() => void openPrs()}
              >
                {busy
                  ? "Working…"
                  : `Push & open ${pending.length} PR${pending.length === 1 ? "" : "s"}`}
              </button>
            </>
          }
        >
          {repoRows}

          {pending.length === 0 ? (
            <div className="muted" style={{ lineHeight: 1.6 }}>
              Every repository with changes already has an open pull request.
            </div>
          ) : (
            <>
              <Field label="Title">
                <input value={title} onChange={(e) => setTitle(e.target.value)} />
              </Field>
              <Field
                label="Description"
                hint={
                  drafting
                    ? "Writing — it appears here as it goes."
                    : "Written from the diff by a fast model, as a handoff for whoever reviews this."
                }
              >
                <textarea rows={8} value={body} onChange={(e) => setBody(e.target.value)} />
                <div className="row" style={{ marginTop: 8 }}>
                  <button
                    className="btn btn-sm"
                    disabled={drafting}
                    title="Write the description from the diff on this branch"
                    onClick={() => void draftWithAgent()}
                  >
                    {drafting ? "Drafting…" : "✨ Draft description"}
                  </button>
                  {drafting && <Spinner />}
                </div>
              </Field>
              <div className="row" style={{ marginBottom: 12 }}>
                <label className="row" style={{ gap: 6, cursor: "pointer" }}>
                  <input
                    type="checkbox"
                    style={{ width: "auto" }}
                    checked={draft}
                    onChange={(e) => setDraft(e.target.checked)}
                  />
                  Open as draft
                </label>
              </div>
            </>
          )}

          <div className="muted" style={{ lineHeight: 1.55 }}>
            Opens one PR per repository with changes, all from <code>{task.branch}</code>.
            {task.issue_key
              ? ` The links are then posted back to ${task.issue_key} as a single comment, so the ticket is where the set stays joined up.`
              : ""}
          </div>
        </Modal>
      )}
    </div>
  );
}
