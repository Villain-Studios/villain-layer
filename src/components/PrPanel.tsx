import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api } from "../lib/api";
import { reviewComments, taskReview, useStore, type TaskReview } from "../store";
import type { CheckoutPr, CheckRun, RepoResult, Review, TaskView } from "../lib/types";
import { Field, Spinner } from "./ui";

/**
 * The latest review from each reviewer, which is the one GitHub itself shows.
 *
 * Reviewers come back and change their minds; listing every submission would
 * show a PR as both approved and blocked by the same person.
 */
function latestByAuthor(reviews: Review[]): Review[] {
  const by = new Map<string, Review>();
  for (const r of reviews) by.set(r.author, r);
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

function checkColor(c: CheckRun) {
  if (c.status !== "completed") return "var(--amber)";
  if (c.conclusion === "success") return "var(--green)";
  if (c.conclusion === "skipped" || c.conclusion === "neutral") return "var(--dim)";
  return "var(--red)";
}

export function PrPanel({ task }: { task: TaskView }) {
  const settings = useStore((s) => s.settings);
  const allPanes = useStore((s) => s.panes);
  const agentPanes = useMemo(
    () => allPanes.filter((p) => p.task_id === task.id && p.kind === "agent" && p.running),
    [allPanes, task.id],
  );
  const toggleSettings = useStore((s) => s.toggleSettings);
  const toast = useStore((s) => s.toast);
  const refreshPrs = useStore((s) => s.refreshPrs);
  const fail = useStore((s) => s.fail);

  const watched = useStore((s) => s.prs[task.id]);
  const [rows, setRows] = useState<CheckoutPr[]>([]);
  /**
   * Whether GitHub has answered yet.
   *
   * Without this the panel renders its empty state first — the form for
   * opening a PR — and then replaces it with the PR that was there all along.
   * An answer that arrives a moment later is not a reason to show the wrong
   * one in the meantime.
   */
  const [loaded, setLoaded] = useState(false);
  const [branches, setBranches] = useState<Record<string, string[]>>({});
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
      setRows(await api.githubTaskPrs(task.id));
      setLoaded(true);
    } catch (e) {
      fail(e);
      // An error is an answer too: leaving this false would hide the form
      // for good and offer nothing in its place.
      setLoaded(true);
    } finally {
      setLoading(false);
    }
  }, [task.id, settings?.github_connected, fail]);

  useEffect(() => { void load(); }, [load]);

  // The watch refreshes every task on its own timer. Taking what it saw keeps
  // an open panel current without it having to poll on its own account.
  useEffect(() => {
    if (!watched) return;
    setRows(watched);
    setLoaded(true);
  }, [watched]);

  // What the base field offers. Keyed on the set of repos rather than the rows
  // themselves, so a sweep landing every ninety seconds does not refetch it.
  const checkoutIds = rows.map((r) => r.checkout_id).join(",");
  useEffect(() => {
    let stop = false;
    void (async () => {
      for (const id of checkoutIds.split(",").filter(Boolean)) {
        try {
          const list = await api.checkoutBranches(id);
          if (stop) return;
          setBranches((b) => ({ ...b, [id]: list }));
        } catch {
          // A repo whose branches cannot be listed still takes a typed base.
        }
      }
    })();
    return () => { stop = true; };
  }, [checkoutIds]);

  // Stop waiting for a draft if the panel goes away.
  useEffect(() => () => { if (pollRef.current) window.clearInterval(pollRef.current); }, []);

  // The description arrives a few words at a time. Showing it as it is written
  // is most of what makes this feel quick: the wait is the same, but it starts
  // reading like an answer straight away instead of a spinner.
  useEffect(() => {
    const p = listen<{ task_id: string; text: string }>("pr:draft", (e) => {
      if (e.payload.task_id !== task.id) return;
      setBody((current) => current + e.payload.text);
    });
    return () => { void p.then((un) => un()); };
  }, [task.id]);

  useEffect(() => {
    setTitle(task.name);
    setBody(task.issue_url ? `Jira: ${task.issue_url}\n` : "");
  }, [task.id, task.name, task.issue_url]);

  function report(results: RepoResult[], verb: string) {
    const bad = results.filter((r) => !r.ok);
    const ok = results.filter((r) => r.ok);
    if (bad.length) toast("error", bad.map((r) => `${r.repo}: ${r.detail}`).join("\n"));
    if (ok.length) {
      toast("success", `${verb} ${ok.map((r) => `${r.repo} (${r.detail})`).join(", ")}`);
    }
  }

  function openPr(url: string) {
    void openUrl(url).catch(() => toast("error", "Could not open the browser"));
  }

  function copyPr(url: string, number: number) {
    navigator.clipboard
      .writeText(url)
      .then(() => toast("success", `Copied the link to #${number}`))
      .catch(() => toast("error", "Could not reach the clipboard"));
  }

  /** Where this repo's next PR goes. Changing it leaves an open PR alone. */
  async function setBase(row: CheckoutPr, base: string) {
    if (base.trim() === row.base || !base.trim()) return;
    try {
      await api.setCheckoutBase(row.checkout_id, base);
      // Both copies, or the watch's older rows would put the old base back.
      await load();
      await refreshPrs();
    } catch (e) {
      fail(e);
    }
  }

  async function retarget(row: CheckoutPr) {
    try {
      toast("success", await api.githubRetargetPr(row.checkout_id));
      await load();
      await refreshPrs();
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
    try {
      setBody((await api.draftPrDescription(task.id)).trim());
      toast("success", "Description drafted — edit it before opening the PR.");
      setDrafting(false);
      return;
    } catch (e) {
      // Whatever was streamed before it failed is half an answer, not an answer.
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
      report(await api.pushTask(task.id), "Pushed");
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
      report(await api.githubOpenPrs(task.id, title.trim(), body, draft), "Opened");
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
  // Only an open PR is worth a card. One merged or closed is an outcome, not
  // something to read twenty check runs about.
  const live = rows.filter((r) => r.pr && r.pr.state === "open");
  const done = rows.filter((r) => r.pr && r.pr.state !== "open");
  const review = taskReview(rows);
  const said = reviewComments(rows);

  return (
    <div className="panel-scroll">
      {loading && <div className="row" style={{ marginBottom: 10 }}><Spinner /> Loading…</div>}

      {review !== "none" && (
        <div className="row" style={{ marginBottom: 10 }}>
          <span className="dot" style={{ background: TASK_REVIEW[review].color }} />
          <b>{TASK_REVIEW[review].word}</b>
          <span className="muted">
            {live.length} PR{live.length === 1 ? "" : "s"}
            {/* A task is only as reviewed as its least reviewed repository. */}
            {review === "incomplete" &&
              `, ${pending.length} repo${pending.length === 1 ? "" : "s"} still without one`}
            {said > 0 && ` · ${said} comment${said === 1 ? "" : "s"}`}
          </span>
        </div>
      )}

      {live.map((row) => (
        <div key={row.checkout_id} className="card">
          <div className="row">
            <h3 style={{ margin: 0 }}>
              <span style={{ color: "var(--dim)" }}>{row.repo}</span>{" "}
              #{row.pr!.number} {row.pr!.title}
            </h3>
            <div className="spacer" />
            {row.pr!.draft && <span className="chip">draft</span>}
            {row.pr!.merged && <span className="chip add">merged</span>}
            {row.verdict === "approved" && <span className="chip add">approved</span>}
            {row.verdict === "changes_requested" && (
              <span className="chip del">changes requested</span>
            )}
            <span className="chip">{row.pr!.state}</span>
            {/*
              At the top, not under the checks: a PR with a dozen check runs
              put its only link below the fold, which is no link at all.
            */}
            <button className="btn-sm" title="Open on GitHub" onClick={() => openPr(row.pr!.url)}>
              ↗
            </button>
            <button
              className="btn-sm"
              title="Copy link"
              onClick={() => copyPr(row.pr!.url, row.pr!.number)}
            >
              ⧉
            </button>
          </div>
          <div className="muted" style={{ marginTop: 6 }}>
            {row.pr!.head} → {row.pr!.base} · opened by {row.pr!.author}
            {row.pr!.comments + row.pr!.review_comments > 0 && (
              <>
                {" · "}
                {row.pr!.comments + row.pr!.review_comments} comment
                {row.pr!.comments + row.pr!.review_comments === 1 ? "" : "s"}
              </>
            )}
          </div>

          {row.pr!.base !== row.base && row.pr!.state === "open" && (
            <div className="row" style={{ marginTop: 10 }}>
              <span className="chip warn">targets {row.pr!.base}</span>
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
                    <button className="btn-sm" onClick={() => void openUrl(r.url)}>↗</button>
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
                  {c.url && <button className="btn-sm" onClick={() => void openUrl(c.url!)}>↗</button>}
                </div>
              ))}
            </div>
          )}

          <div className="row" style={{ marginTop: 10 }}>
            <button className="btn btn-sm" onClick={() => openPr(row.pr!.url)}>
              Open on GitHub
            </button>
          </div>
        </div>
      ))}

      {done.length > 0 && (
        <div className="card">
          <h3>Already decided</h3>
          {done.map((row) => (
            <div key={row.checkout_id} className="check">
              <span
                className="dot"
                style={{ background: row.pr!.merged ? "var(--green)" : "var(--dim)" }}
              />
              <span className="name">
                {row.repo} #{row.pr!.number}
              </span>
              <span className="muted">
                {row.pr!.merged
                  ? `merged into ${row.pr!.base}`
                  : "closed without merging"}
              </span>
              <button className="btn-sm" title="Open on GitHub" onClick={() => openPr(row.pr!.url)}>
                ↗
              </button>
              <button
                className="btn-sm"
                title="Copy link"
                onClick={() => copyPr(row.pr!.url, row.pr!.number)}
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

      {loaded && (
      <div className="card">
        <h3>
          {pending.length === 0 && live.length > 0
            ? "Everything with changes has a PR"
            : `Open pull request${pending.length === 1 ? "" : "s"}`}
        </h3>

        {rows.length > 0 && (
          <div className="muted" style={{ marginBottom: 10, lineHeight: 1.6 }}>
            {rows.map((r) => (
              <div key={r.checkout_id} className="row">
                <span className="dot" style={{
                  background: r.pr ? "var(--green)" : r.changed > 0 ? "var(--amber)" : "var(--dimmer)",
                }} />
                <span style={{ fontFamily: "var(--mono)", fontSize: 11.5, width: 140 }}>
                  {r.repo}
                </span>
                <span>
                  {r.pr ? `PR #${r.pr.number}` : r.changed > 0
                    ? `${r.changed} changed file${r.changed === 1 ? "" : "s"}`
                    : "no changes"}
                </span>
                <div className="spacer" />
                <span>→</span>
                <input
                  list={`branches-${r.checkout_id}`}
                  style={{ width: 240 }}
                  defaultValue={r.base}
                  title="The branch this repository's pull request is opened against"
                  onBlur={(e) => void setBase(r, e.target.value)}
                  onKeyDown={(e) => { if (e.key === "Enter") e.currentTarget.blur(); }}
                />
                {/*
                  Typing still works: the list is the worktree's remote-tracking
                  refs, so a branch pushed since the last fetch is not in it.
                */}
                <datalist id={`branches-${r.checkout_id}`}>
                  {(branches[r.checkout_id] ?? []).map((b) => (
                    <option key={b} value={b} />
                  ))}
                </datalist>
              </div>
            ))}
          </div>
        )}

        {pending.length > 0 && (
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

        <div className="row">
          <button className="btn" disabled={busy} onClick={() => void push()}>
            Push all
          </button>
          <button className="btn btn-sm" onClick={() => void load()}>Refresh</button>
          <div className="spacer" />
          <button
            className="btn btn-primary"
            disabled={busy || pending.length === 0 || !title.trim()}
            onClick={() => void openPrs()}
          >
            {busy
              ? "Working…"
              : `Push & open ${pending.length} PR${pending.length === 1 ? "" : "s"}`}
          </button>
        </div>

        <div className="muted" style={{ marginTop: 10, lineHeight: 1.55 }}>
          Opens one PR per repo with changes, all from <code>{task.branch}</code>.
          {task.issue_key
            ? ` The links are then posted back to ${task.issue_key} as a single comment, so the ticket is where the set stays joined up.`
            : ""}
        </div>
      </div>
      )}
    </div>
  );
}
