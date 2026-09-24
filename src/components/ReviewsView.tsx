import { useEffect, useState, type MouseEvent, type ReactNode } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { copyText } from "../lib/clipboard";
import { ago } from "../lib/time";
import { useStore } from "../store";
import type { ReviewRequest, TeamReviews } from "../lib/types";
import { ContextMenu, Spinner, type MenuItem } from "./ui";

/** `@fe` when that is all we know; the team's own name when GitHub sent one. */
function teamLabel(team: TeamReviews): string {
  const slugPart = team.slug.split("/").pop() || team.slug;
  if (team.name && team.name.toLowerCase() !== slugPart.toLowerCase()) return team.name;
  return `@${slugPart}`;
}

function countOf(n: number, more: boolean): string {
  return more ? `${n}+` : String(n);
}

/** How a banner names a pull request: `owner/repo#n`, as `news.rs` writes it. */
function identity(pr: ReviewRequest): string {
  return pr.repo ? `${pr.repo}#${pr.number}` : `#${pr.number}`;
}

function ReviewCard({
  pr,
  focused,
  onContextMenu,
}: {
  pr: ReviewRequest;
  focused: boolean;
  onContextMenu: (e: MouseEvent) => void;
}) {
  const fail = useStore((s) => s.fail);
  return (
    <button
      type="button"
      className={`review-card${focused ? " focused" : ""}`}
      data-review={identity(pr)}
      onClick={() => void openUrl(pr.url).catch(fail)}
      onContextMenu={onContextMenu}
      title={pr.url}
    >
      <div className="top">
        <span className="repo">{pr.repo || "pull request"}</span>
        <span className="num">#{pr.number}</span>
        {pr.draft && <span className="chip">draft</span>}
        <span className="spacer" />
        <span className="when">{ago(pr.updated_at)}</span>
      </div>
      <div className="title">{pr.title}</div>
      {pr.author && <div className="by">{pr.author}</div>}
    </button>
  );
}

function ReviewList({
  heading,
  title,
  count,
  more,
  open,
  onToggle,
  children,
}: {
  heading: string;
  title?: string;
  count?: string;
  more?: boolean;
  open: boolean;
  onToggle: () => void;
  children: ReactNode;
}) {
  return (
    <section className="review-list" title={title}>
      <button type="button" className="review-list-head" onClick={onToggle}>
        <span className={`chev${open ? " open" : ""}`}>▶</span>
        <span className="title">{heading}</span>
        {count !== undefined && <span className="count">{count}</span>}
      </button>
      {open && (
        <div className="review-list-body">
          {children}
          {more && (
            <p className="review-note">Showing the 100 most recently updated.</p>
          )}
        </div>
      )}
    </section>
  );
}

export function ReviewsView() {
  const settings = useStore((s) => s.settings);
  const queue = useStore((s) => s.reviewQueue);
  const loading = useStore((s) => s.reviewQueueLoading);
  const error = useStore((s) => s.reviewQueueError);
  const refresh = useStore((s) => s.refreshReviewQueue);
  const toggleSettings = useStore((s) => s.toggleSettings);
  const toast = useStore((s) => s.toast);
  const fail = useStore((s) => s.fail);
  // Both start open. Folding is for getting one queue out of the way, not the
  // default: the personal list is why the tab exists.
  const [shut, setShut] = useState<Record<string, boolean>>({});
  const [menu, setMenu] = useState<{ x: number; y: number; items: MenuItem[] } | null>(null);
  const toggle = (id: string) => setShut((c) => ({ ...c, [id]: !c[id] }));
  const focusReview = useStore((s) => s.focusReview);
  const clearFocusReview = useStore((s) => s.clearFocusReview);

  // A banner or a message about one pull request: open its list, bring it
  // into view and light it up briefly. It waits for the queue to arrive; one
  // no longer waiting on anybody simply is not there to find.
  useEffect(() => {
    if (!focusReview || !queue) return;
    const inTeam = queue.team?.prs.some((pr) => identity(pr) === focusReview);
    const inMine = queue.mine.some((pr) => identity(pr) === focusReview);
    if (inMine || inTeam) setShut((c) => ({ ...c, [inMine ? "you" : "team"]: false }));
    const scroll = window.setTimeout(() => {
      document
        .querySelector(`[data-review="${CSS.escape(focusReview)}"]`)
        ?.scrollIntoView({ block: "center", behavior: "smooth" });
    }, 80);
    const clear = window.setTimeout(() => clearFocusReview(), 2200);
    return () => {
      window.clearTimeout(scroll);
      window.clearTimeout(clear);
    };
  }, [focusReview, queue, clearFocusReview]);

  function copy(text: string, what: string) {
    void copyText(text)
      .then(() => toast("success", `Copied ${what}`))
      .catch(() => toast("error", "Could not reach the clipboard"));
  }

  function prMenu(pr: ReviewRequest): MenuItem[] {
    const key = identity(pr);
    return [
      { label: "Open in GitHub", onSelect: () => void openUrl(pr.url).catch(fail) },
      { label: "Copy link", onSelect: () => copy(pr.url, pr.url) },
      { label: `Copy ${key}`, onSelect: () => copy(key, key) },
      { label: "Copy title", onSelect: () => copy(pr.title, key) },
    ];
  }

  function openMenu(e: MouseEvent, pr: ReviewRequest) {
    e.preventDefault();
    setMenu({ x: e.clientX, y: e.clientY, items: prMenu(pr) });
  }

  if (!settings) {
    return (
      <div className="empty">
        <h2>Loading…</h2>
      </div>
    );
  }

  if (!settings.github_connected) {
    return (
      <div className="empty">
        <h2>GitHub not connected</h2>
        <p>Connect GitHub to list pull requests waiting on your review.</p>
        <button type="button" className="btn" onClick={() => toggleSettings(true)}>Connect GitHub</button>
      </div>
    );
  }

  const team = queue?.team ?? null;

  return (
    <div className="wide">
      <div className="wide-head">
        <h2>Reviews</h2>
        <span className="sub">Pull requests waiting on a review</span>
        <div className="spacer" />
        {loading && <Spinner />}
        <button type="button" className="btn btn-sm" onClick={() => void refresh()}>Refresh</button>
      </div>

      {error && <p className="review-note warn">{error}</p>}

      <ReviewList
        heading="You"
        count={queue ? countOf(queue.mine.length, queue.mine_more) : undefined}
        more={queue?.mine_more}
        open={!shut.you}
        onToggle={() => toggle("you")}
      >
        {queue && queue.mine.length === 0 && (
          <p className="review-note">Nothing is waiting on you.</p>
        )}
        {queue?.mine.map((pr) => (
          <ReviewCard
            key={`${pr.repo}#${pr.number}`}
            pr={pr}
            focused={focusReview === identity(pr)}
            onContextMenu={(e) => openMenu(e, pr)}
          />
        ))}
        {!queue && loading && <p className="review-note">Loading…</p>}
      </ReviewList>

      {team ? (
        <ReviewList
          heading={teamLabel(team)}
          title={team.slug}
          count={team.error ? undefined : countOf(team.prs.length, team.more)}
          more={team.more && !team.error}
          open={!shut.team}
          onToggle={() => toggle("team")}
        >
          {team.error ? (
            <p className="review-note warn">{team.error}</p>
          ) : team.prs.length === 0 ? (
            <p className="review-note">Nothing is waiting on {teamLabel(team)}.</p>
          ) : (
            team.prs.map((pr) => (
              <ReviewCard
                key={`${pr.repo}#${pr.number}`}
                pr={pr}
                focused={focusReview === identity(pr)}
                onContextMenu={(e) => openMenu(e, pr)}
              />
            ))
          )}
        </ReviewList>
      ) : (
        <ReviewList heading="Team" open={!shut.team} onToggle={() => toggle("team")}>
          <p className="review-note">
            Set a review team in Settings → GitHub — for example @fe — to list
            pull requests requested of that team.
          </p>
          <button type="button" className="btn btn-sm" onClick={() => toggleSettings(true)}>Settings</button>
        </ReviewList>
      )}

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          items={menu.items}
          onClose={() => setMenu(null)}
        />
      )}
    </div>
  );
}
