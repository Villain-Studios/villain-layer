//! Banners for a review requested or a ticket assigned, while the window is
//! in the background.
//!
//! These were the webview's, on a timer it ran for them — and a hidden
//! webview is the one whose timers macOS throttles or stops, so the banners
//! were least reliable exactly when they were the point. The backend now
//! keeps what has been seen, from every fetch the UI makes and from one of
//! its own while the window is away, and says what is new.

use std::collections::HashSet;
use std::time::Duration;

use tauri::{AppHandle, Manager};

use crate::commands::{banner, AppState, ReviewQueue};
use crate::messages::{self, Kind, Level, New};
use crate::target::Target;

/// How often to look while the window is away. The same three minutes the
/// webview used.
const EVERY: Duration = Duration::from_secs(180);
/// Past this many at once, one banner rather than a stack of them.
const BATCH: usize = 3;

/// What has been seen, so only what is new is news. None until the first
/// snapshot, which is recorded and not announced: opening the app would
/// otherwise banner every pull request and ticket already waiting.
#[derive(Default)]
pub struct Seen {
    reviews: parking_lot::Mutex<Option<HashSet<String>>>,
    /// The team list's first arrival is a snapshot too, whenever it comes.
    team_primed: parking_lot::Mutex<bool>,
    tickets: parking_lot::Mutex<Option<HashSet<String>>>,
}

fn identity(pr: &crate::integrations::github::ReviewRequest) -> String {
    if pr.repo.is_empty() {
        format!("#{}", pr.number)
    } else {
        format!("{}#{}", pr.repo, pr.number)
    }
}

impl Seen {
    /// The pull requests in `queue` not seen before, remembering them.
    ///
    /// A team lookup that failed comes back with nothing in it. Forgetting
    /// what it held would make the next success announce all of it again.
    fn reviews(&self, queue: &ReviewQueue) -> Vec<(String, String)> {
        let mut seen = self.reviews.lock();
        let mut primed = self.team_primed.lock();
        let team_ok = queue.team.as_ref().is_some_and(|t| t.error.is_none());

        let mut next: HashSet<String> = queue.mine.iter().map(identity).collect();
        match &queue.team {
            Some(t) if t.error.is_none() => next.extend(t.prs.iter().map(identity)),
            Some(_) => {
                if let Some(prev) = seen.as_ref() {
                    next.extend(prev.iter().cloned());
                }
            }
            None => *primed = false,
        }

        let mut fresh: Vec<&crate::integrations::github::ReviewRequest> = Vec::new();
        if let Some(prev) = seen.as_ref() {
            fresh.extend(queue.mine.iter().filter(|pr| !prev.contains(&identity(pr))));
            if let (true, Some(t)) = (team_ok, &queue.team) {
                if *primed {
                    fresh.extend(t.prs.iter().filter(|pr| !prev.contains(&identity(pr))));
                }
            }
        }
        if team_ok {
            *primed = true;
        }
        *seen = Some(next);
        fresh.into_iter().map(|pr| (identity(pr), pr.title.clone())).collect()
    }

    /// The tickets in `issues` not seen before, remembering them.
    fn tickets(&self, issues: &[crate::integrations::jira::Issue]) -> Vec<(String, String)> {
        let mut seen = self.tickets.lock();
        let fresh = match seen.as_ref() {
            Some(prev) => issues
                .iter()
                .filter(|i| !prev.contains(&i.key))
                .map(|i| (i.key.clone(), i.summary.clone()))
                .collect(),
            None => Vec::new(),
        };
        *seen = Some(issues.iter().map(|i| i.key.clone()).collect());
        fresh
    }

    /// Start again from a snapshot: the connection changed, and the old
    /// list says nothing about the new one.
    ///
    /// Not when banners are turned off, which it once was: the message
    /// center keeps these either way (MSG-2), and a snapshot every three
    /// minutes made nearly every new ticket part of one.
    pub fn forget(&self) {
        *self.reviews.lock() = None;
        *self.team_primed.lock() = false;
        *self.tickets.lock() = None;
    }
}

/// Banners are on, and nobody is looking at the window.
fn announce_now(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    state.config.read().ui.system_notifications
        && app
            .get_webview_window("main")
            .is_some_and(|w| !w.is_focused().unwrap_or(false))
}

/// A review queue has come in, from wherever it was asked for.
pub fn saw_reviews(app: &AppHandle, queue: &ReviewQueue) {
    let fresh = app.state::<AppState>().news.reviews(queue);
    let news = review_news(&fresh);
    messages::record_all(app, news.clone());
    if news.is_empty() || !announce_now(app) {
        return;
    }
    for (title, body, target) in banners(news, "Reviews", "pull requests are waiting on a review", Target::Reviews) {
        let _ = banner(app, title, body, target);
    }
}

/// Your ticket list has come in, from wherever it was asked for.
pub fn saw_tickets(app: &AppHandle, issues: &[crate::integrations::jira::Issue]) {
    let fresh = app.state::<AppState>().news.tickets(issues);
    let news = ticket_news(&fresh);
    messages::record_all(app, news.clone());
    if news.is_empty() || !announce_now(app) {
        return;
    }
    for (title, body, target) in banners(news, "Tickets", "tickets were assigned to you", Target::Tickets) {
        let _ = banner(app, title, body, target);
    }
}

/// New review requests, as the message center keeps them.
fn review_news(fresh: &[(String, String)]) -> Vec<New> {
    fresh
        .iter()
        .map(|(pr, title)| New {
            kind: Kind::Review,
            level: Level::Info,
            title: "Review requested".into(),
            body: format!("{pr} — {title}"),
            target: Some(Target::Review(pr).to_string()),
        })
        .collect()
}

/// New tickets, as the message center keeps them.
fn ticket_news(fresh: &[(String, String)]) -> Vec<New> {
    fresh
        .iter()
        .map(|(key, summary)| New {
            kind: Kind::Ticket,
            level: Level::Info,
            title: "New ticket".into(),
            body: format!("{key} — {summary}"),
            target: Some(Target::Ticket(key).to_string()),
        })
        .collect()
}

/// A banner each, or one for the lot past `BATCH`: title, body and what a
/// click opens.
fn banners(news: Vec<New>, heading: &str, many: &str, all: Target) -> Vec<(String, String, String)> {
    if news.len() > BATCH {
        return vec![(heading.into(), format!("{} {many}", news.len()), all.to_string())];
    }
    news.into_iter()
        .map(|n| (n.title, n.body, n.target.unwrap_or_else(|| all.to_string())))
        .collect()
}

/// Look for news while the window is away, for as long as the app runs.
///
/// While it is in front the UI's own fetches keep `Seen` current, and
/// nothing needs saying.
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(EVERY).await;
            let state = app.state::<AppState>();
            // Only while away with banners on: in front, the UI's own
            // fetches keep `Seen` current and record what is new.
            if !announce_now(&app) {
                continue;
            }
            let (github, jira) = {
                let cfg = state.config.read();
                (cfg.github.is_some(), cfg.jira.is_some())
            };
            if github {
                if let Ok(queue) = crate::commands::review_queue(&state).await {
                    saw_reviews(&app, &queue);
                }
            }
            if jira {
                if let Ok(page) = crate::commands::my_issues(&state).await {
                    saw_tickets(&app, &page.issues);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::TeamReviews;
    use crate::integrations::github::ReviewRequest;

    fn pr(repo: &str, number: u64) -> ReviewRequest {
        ReviewRequest {
            repo: repo.into(),
            number,
            title: format!("t{number}"),
            url: String::new(),
            author: String::new(),
            draft: false,
            updated_at: String::new(),
        }
    }

    fn queue(mine: Vec<ReviewRequest>, team: Option<(Vec<ReviewRequest>, bool)>) -> ReviewQueue {
        ReviewQueue {
            mine,
            mine_more: false,
            team: team.map(|(prs, failed)| TeamReviews {
                slug: "o/t".into(),
                name: "t".into(),
                prs,
                more: false,
                error: failed.then(|| "boom".into()),
            }),
        }
    }

    #[test]
    fn the_first_look_is_a_snapshot_and_after_it_only_new_is_news() {
        let seen = Seen::default();
        assert!(seen.reviews(&queue(vec![pr("a", 1)], None)).is_empty());
        assert!(seen.reviews(&queue(vec![pr("a", 1)], None)).is_empty());
        let fresh = seen.reviews(&queue(vec![pr("a", 1), pr("a", 2)], None));
        assert_eq!(fresh, [("a#2".to_string(), "t2".to_string())]);
    }

    #[test]
    fn a_team_list_arriving_later_is_a_snapshot_and_a_failed_one_forgets_nothing() {
        let seen = Seen::default();
        seen.reviews(&queue(vec![], None));
        // The team is configured after the first look: its list is not news.
        assert!(seen.reviews(&queue(vec![], Some((vec![pr("b", 5)], false)))).is_empty());
        // A failed lookup keeps what was known ...
        assert!(seen.reviews(&queue(vec![], Some((vec![], true)))).is_empty());
        // ... so the next success does not announce it all again.
        let fresh = seen.reviews(&queue(vec![], Some((vec![pr("b", 5), pr("b", 6)], false))));
        assert_eq!(fresh.iter().map(|f| f.0.as_str()).collect::<Vec<_>>(), ["b#6"]);
    }

    #[test]
    fn a_ticket_is_news_once() {
        let seen = Seen::default();
        let issue = |key: &str| crate::integrations::jira::Issue {
            key: key.into(),
            summary: format!("s {key}"),
            ..Default::default()
        };
        assert!(seen.tickets(&[issue("ACME-1")]).is_empty());
        assert_eq!(seen.tickets(&[issue("ACME-1"), issue("ACME-2")]).len(), 1);
        assert!(seen.tickets(&[issue("ACME-1"), issue("ACME-2")]).is_empty());
        seen.forget();
        assert!(seen.tickets(&[issue("ACME-3")]).is_empty(), "after forgetting, a snapshot again");
    }

    #[test]
    fn a_new_review_opens_that_pull_request() {
        let say = |fresh: &[(String, String)]| {
            banners(review_news(fresh), "Reviews", "waiting", Target::Reviews)
        };
        let one = [("o/a#7".to_string(), "Fix it".to_string())];
        assert_eq!(say(&one), [("Review requested".into(), "o/a#7 — Fix it".into(), "review:o/a#7".into())]);
        let many: Vec<_> = (0..4).map(|i| (format!("o/a#{i}"), String::new())).collect();
        assert_eq!(say(&many)[0].2, "reviews");
        let ticket = [("ACME-3".to_string(), "Do it".to_string())];
        assert_eq!(ticket_news(&ticket)[0].target.as_deref(), Some("ticket:ACME-3"));
    }

    #[test]
    fn every_new_review_is_kept_even_when_its_banner_is_one_of_many() {
        let many: Vec<_> = (0..5).map(|i| (format!("o/a#{i}"), String::new())).collect();
        assert_eq!(review_news(&many).len(), 5);
    }
}
