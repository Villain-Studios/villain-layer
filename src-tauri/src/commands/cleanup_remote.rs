//! Clean up's branches on origin (REPO-8): yours, and done with, by what
//! GitHub says became of the pull requests they came from.
//!
//! Git alone cannot tell a branch squash-merged from one never merged: the
//! squash made new commits, and the branch's own are on no other branch.
//! GitHub can, so it is asked, and only about repos on the GitHub the app
//! is connected to. What it says judges the branches in the app's copies
//! too ([`judge_local`]).

use std::collections::HashMap;
use std::path::PathBuf;

use futures_util::StreamExt;
use tauri::{AppHandle, Manager};

use crate::git::{self, OriginBranch};
use crate::integrations::github::{BranchPr, PrState, PullRequest, MY_PULLS_LIMIT};

use super::cleanup::{in_use, Action, CleanupItem, Planned, Verdict};
use super::github::github_client;
use super::repos::store_of;
use super::AppState;

/// How many pull request lookups are in flight at once: GitHub turns away
/// a burst of concurrent requests as abuse.
const AT_ONCE: usize = 8;

/// One repo, as git sees it: the branches on origin no task uses, who
/// commits here, and where to ask GitHub.
struct Repo {
    name: String,
    store: PathBuf,
    base: String,
    slug: (String, String),
    branches: Vec<OriginBranch>,
    me: Option<String>,
    /// The pull requests the copy's review branches came from.
    reviewed: Vec<u64>,
}

/// What GitHub said about the pull requests behind one app's copy's own
/// branches.
#[derive(Default, Clone)]
pub(super) struct Pulls {
    /// Yours, by the branch each came from; none when GitHub could not say.
    pub(super) mine: Option<Vec<BranchPr>>,
    /// More of yours exist than were read.
    pub(super) more: bool,
    /// By number, the pull requests the app's review branches
    /// (`review/<repo>-<n>`) were checked out from: what became of each and
    /// the commit it ends at, or why GitHub could not say.
    pub(super) reviewed: HashMap<u64, std::result::Result<(PrState, String), String>>,
}

/// Clean up's look at GitHub: the branches on origin it offers, and, by
/// app's copy, what it learned for judging the copy's own branches.
#[derive(Default)]
pub(super) struct Remote {
    pub(super) planned: Vec<Planned>,
    pub(super) pulls: HashMap<PathBuf, Pulls>,
}

/// A branch judged done with, waiting on whether a pull request is open
/// from it.
struct Candidate {
    repo: String,
    store: PathBuf,
    slug: (String, String),
    branch: OriginBranch,
    verdict: Verdict,
    detail: String,
}

/// Branches on origin that are yours and done with, each with why, and a
/// blocked row for each repo GitHub could not be asked about. Nothing
/// without a GitHub connection: there is nobody to ask.
pub(super) async fn plan(app: &AppHandle) -> Remote {
    let Ok((client, cfg)) = github_client(&app.state::<AppState>()) else { return Remote::default() };
    let host = |url: &str| git::remote_key(url).split('/').next().unwrap_or_default().to_string();
    let ours = host(&cfg.web_url);

    let looked = super::blocking(app.clone(), move |state| {
        let cfg = state.config.read();
        let used = in_use(state, &cfg);
        let mut repos = Vec::new();
        let mut out = Vec::new();
        for project in &cfg.projects {
            let Some(store) = store_of(project) else { continue };
            // Another host's pull requests are not on this GitHub, and a
            // branch whose open one went unseen would be deleted under it.
            if git::origin_url(store).is_none_or(|u| host(&u) != ours) {
                continue;
            }
            let found = git::origin_slug(store).and_then(|slug| Ok((slug, git::origin_branches(store, &project.default_branch)?)));
            match found {
                Ok((slug, branches)) => repos.push(Repo {
                    name: project.name.clone(),
                    store: store.to_path_buf(),
                    base: project.default_branch.clone(),
                    slug,
                    branches: branches.into_iter().filter(|b| !used.branches.contains(&b.name)).collect(),
                    me: git::user_email(store),
                    reviewed: git::branch_tips(store)
                        .map(|tips| tips.iter().filter_map(|(b, _)| review_number(b)).collect())
                        .unwrap_or_default(),
                }),
                Err(e) => out.push(problem(&project.name, store.to_path_buf(), e.to_string())),
            }
        }
        Ok((repos, out))
    })
    .await;
    let Ok((repos, mut out)) = looked else { return Remote::default() };
    let client = &client;

    // Your pull requests, a repo at a time in parallel.
    let asked = futures_util::future::join_all(repos.iter().map(|r| client.my_pulls(&r.slug.0, &r.slug.1))).await;
    let mut pulls: HashMap<PathBuf, Pulls> = HashMap::new();
    let mut candidates = Vec::new();
    for (repo, prs) in repos.iter().zip(asked) {
        match prs {
            Ok((prs, more)) => {
                let here = pulls.entry(repo.store.clone()).or_default();
                here.mine = Some(prs.clone());
                here.more = more;
                if more {
                    out.push(Planned {
                        item: CleanupItem {
                            id: format!("origin_branch:{}:more", repo.store.display()),
                            kind: "origin_branch",
                            repo: Some(repo.name.clone()),
                            title: "Older pull requests".into(),
                            detail: format!(
                                "Only your {MY_PULLS_LIMIT} newest pull requests here were read. A branch from an older one is offered only if every commit on it is in {}.",
                                repo.base
                            ),
                            verdict: Verdict::Blocked,
                        },
                        action: Action::OriginBranch(repo.store.clone(), String::new(), String::new()),
                    });
                }
                for (branch, verdict, detail) in judge(&repo.branches, repo.me.as_deref(), &prs, &repo.base) {
                    candidates.push(Candidate {
                        repo: repo.name.clone(),
                        store: repo.store.clone(),
                        slug: repo.slug.clone(),
                        branch: branch.clone(),
                        verdict,
                        detail,
                    });
                }
            }
            Err(e) => out.push(problem(&repo.name, repo.store.clone(), format!("Could not ask GitHub about them: {e}"))),
        }
    }

    // The pull requests the review branches came from: anyone's, so not
    // among yours, and asked one at a time.
    let mut reviews = Vec::new();
    for r in &repos {
        reviews.extend(r.reviewed.iter().map(|n| (r.store.clone(), r.slug.clone(), *n)));
    }
    let looked_up: Vec<_> = futures_util::stream::iter(reviews.into_iter().map(|(store, (owner, name), n)| async move {
        let found = client.pull(&owner, &name, n).await;
        (store, n, found)
    }))
    .buffered(AT_ONCE)
    .collect()
    .await;
    for (store, n, found) in looked_up {
        let became = found.map(|p| (pr_state(&p), p.head_sha)).map_err(|e| e.to_string());
        pulls.entry(store).or_default().reviewed.insert(n, became);
    }

    // Anyone's open pull request from a branch keeps it: deleting the branch
    // would close it. Asked per branch, since GitHub filters by head.
    let checked: Vec<(Candidate, _)> = futures_util::stream::iter(candidates.into_iter().map(|c| async move {
        let open = client.pull_for_branch(&c.slug.0, &c.slug.1, &c.branch.name).await;
        (c, open)
    }))
    .buffered(AT_ONCE)
    .collect()
    .await;
    for (c, open) in checked {
        let (verdict, detail) = match open {
            Ok(None) => (c.verdict, c.detail),
            Ok(Some(_)) => continue,
            Err(e) => (Verdict::Blocked, format!("Could not ask GitHub whether a pull request is open from it: {e}")),
        };
        out.push(Planned {
            item: CleanupItem {
                id: format!("origin_branch:{}:{}:{}", c.store.display(), c.branch.name, c.branch.tip),
                kind: "origin_branch",
                repo: Some(c.repo),
                title: c.branch.name.clone(),
                detail,
                verdict,
            },
            action: Action::OriginBranch(c.store, c.branch.name, c.branch.tip),
        });
    }
    Remote { planned: out, pulls }
}

fn pr_state(p: &PullRequest) -> PrState {
    match (p.merged, p.state.as_str()) {
        (true, _) => PrState::Merged,
        (false, "open") => PrState::Open,
        _ => PrState::Closed,
    }
}

/// The pull request a review branch the app made (`review/<repo>-<n>`, on
/// GitHub's `refs/pull/<n>/head`) was checked out from.
fn review_number(branch: &str) -> Option<u64> {
    branch.strip_prefix("review/")?.rsplit_once('-')?.1.parse().ok()
}

/// What GitHub's pull requests say of a branch in the app's copy that no
/// task uses and that has `n` commits on no origin branch, or nothing when
/// they cannot say. `holds(sha)`: whether commit `sha` has every commit on
/// the branch.
///
/// A pull request merged by squashing leaves its branch's own commits on no
/// branch at all, once origin deletes the branch: git alone takes that for
/// work never pushed, and every finished task's branch was offered unticked.
pub(super) fn judge_local(
    branch: &str,
    n: usize,
    pulls: &Pulls,
    base: &str,
    holds: impl Fn(&str) -> bool,
) -> Option<(Verdict, String)> {
    let not_on_origin = if n == 1 { "1 commit on it is on no origin branch".to_string() } else { format!("{n} commits on it are on no origin branch") };
    if let Some(number) = review_number(branch) {
        return Some(match pulls.reviewed.get(&number)? {
            Ok((_, head)) if holds(head) => (
                Verdict::Safe,
                format!("Left from reviewing #{number}. GitHub keeps every commit on it in that pull request."),
            ),
            Ok(_) => (
                Verdict::Risky,
                format!("Left from reviewing #{number}, but this copy cannot tell that the pull request has every commit on it: some may have been made while reviewing."),
            ),
            Err(e) => (Verdict::Risky, format!("Left from reviewing #{number}, and {not_on_origin}. GitHub could not say what became of it: {e}")),
        });
    }
    let mine = pulls.mine.as_ref()?;
    let Some(p) = mine.iter().filter(|p| p.head == branch).max_by_key(|p| p.number) else {
        let none = if pulls.more {
            format!("none of your {MY_PULLS_LIMIT} newest pull requests here came from it")
        } else {
            "no pull request of yours came from it".to_string()
        };
        return Some((Verdict::Risky, format!("No task uses it, {none}, and {not_on_origin}.")));
    };
    let all = holds(&p.head_sha);
    Some(match p.state {
        PrState::Merged if all => (
            Verdict::Safe,
            format!("No task uses it, and #{} merged every commit on it. GitHub can restore it from the pull request.", p.number),
        ),
        PrState::Merged => (
            Verdict::Risky,
            format!("#{} merged, but this copy cannot tell that it merged every commit on the branch: some may have come after.", p.number),
        ),
        PrState::Open => (Verdict::Risky, format!("#{} is still open, and {not_on_origin}: never pushed.", p.number)),
        PrState::Closed => (
            Verdict::Risky,
            format!(
                "#{} was closed without merging: its work is not in {base}.{}",
                p.number,
                if all { " GitHub can restore what the pull request saw." } else { "" }
            ),
        ),
    })
}

fn problem(repo: &str, store: PathBuf, detail: String) -> Planned {
    Planned {
        item: CleanupItem {
            id: format!("origin_branch:{}:?", store.display()),
            kind: "origin_branch",
            repo: Some(repo.to_string()),
            title: "Branches on origin".into(),
            detail,
            verdict: Verdict::Blocked,
        },
        action: Action::OriginBranch(store, String::new(), String::new()),
    }
}

/// Which of `branches` are yours and done with, and why. Yours: your newest
/// pull request from it is merged or closed, or, with none, you wrote its
/// last commit. An open one of yours keeps it.
fn judge<'a>(
    branches: &'a [OriginBranch],
    me: Option<&str>,
    prs: &[BranchPr],
    base: &str,
) -> Vec<(&'a OriginBranch, Verdict, String)> {
    let mut out = Vec::new();
    for b in branches {
        let newest = prs.iter().filter(|p| p.head == b.name).max_by_key(|p| p.number);
        let judged = match newest {
            Some(p) if p.state == PrState::Open => continue,
            Some(p) if b.in_base => {
                let became = if p.state == PrState::Merged { "merged" } else { "was closed" };
                (Verdict::Safe, format!("#{} {became}, and every commit on it is in {base}.", p.number))
            }
            Some(p) => {
                let since = if p.head_sha == b.tip { "" } else { ", and it was pushed to since" };
                match p.state {
                    PrState::Merged if since.is_empty() => (
                        Verdict::Safe,
                        format!("#{} merged, and nothing was pushed to it since. GitHub can restore it from the pull request.", p.number),
                    ),
                    PrState::Merged => (
                        Verdict::Risky,
                        format!("#{} merged, but it was pushed to since: what came after is not in {base}.", p.number),
                    ),
                    _ => (
                        Verdict::Risky,
                        format!(
                            "#{} was closed without merging{since}: its work is not in {base}. GitHub can restore what the pull request saw.",
                            p.number
                        ),
                    ),
                }
            }
            None if b.in_base && me.is_some_and(|me| me == b.author) => (
                Verdict::Safe,
                format!("No pull request of yours came from it, you wrote its last commit, and every commit on it is in {base}."),
            ),
            None => continue,
        };
        out.push((b, judged.0, judged.1));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn branch(name: &str, tip: &str, author: &str, in_base: bool) -> OriginBranch {
        OriginBranch { name: name.into(), tip: tip.into(), author: author.into(), in_base }
    }

    fn pr(number: u64, head: &str, head_sha: &str, state: PrState) -> BranchPr {
        BranchPr { number, head: head.into(), head_sha: head_sha.into(), state }
    }

    fn verdicts(branches: &[OriginBranch], prs: &[BranchPr]) -> Vec<(String, Verdict)> {
        judge(branches, Some("me@acme.test"), prs, "main").into_iter().map(|(b, v, _)| (b.name.clone(), v)).collect()
    }

    #[test]
    fn a_branch_whose_pull_request_merged_untouched_goes_and_one_pushed_to_since_is_asked_about() {
        let branches = [branch("ACME-1", "aaa", "me@acme.test", false), branch("ACME-2", "bbb", "me@acme.test", false)];
        let prs = [pr(1, "ACME-1", "aaa", PrState::Merged), pr(2, "ACME-2", "b0b", PrState::Merged)];
        assert_eq!(verdicts(&branches, &prs), vec![("ACME-1".into(), Verdict::Safe), ("ACME-2".into(), Verdict::Risky)]);
    }

    #[test]
    fn a_pull_request_closed_unmerged_is_offered_but_never_ticked() {
        let branches = [branch("ACME-3", "ccc", "me@acme.test", false)];
        let found = judge(&branches, None, &[pr(3, "ACME-3", "ccc", PrState::Closed)], "main");
        assert_eq!(found[0].1, Verdict::Risky);
        assert!(found[0].2.contains("closed without merging"));
    }

    #[test]
    fn only_the_newest_pull_request_counts_and_an_open_one_keeps_the_branch() {
        let branches = [branch("ACME-4", "ddd", "me@acme.test", false)];
        let reopened = [pr(4, "ACME-4", "ddd", PrState::Merged), pr(9, "ACME-4", "ddd", PrState::Open)];
        assert!(verdicts(&branches, &reopened).is_empty());
        let retried = [pr(4, "ACME-4", "d0d", PrState::Closed), pr(9, "ACME-4", "ddd", PrState::Merged)];
        assert_eq!(verdicts(&branches, &retried), vec![("ACME-4".into(), Verdict::Safe)]);
    }

    #[test]
    fn without_a_pull_request_only_your_own_branch_already_in_the_base_is_offered() {
        let branches = [
            branch("mine-landed", "eee", "me@acme.test", true),
            branch("mine-wip", "fff", "me@acme.test", false),
            branch("theirs-landed", "ggg", "ana@acme.test", true),
        ];
        assert_eq!(verdicts(&branches, &[]), vec![("mine-landed".into(), Verdict::Safe)]);
        assert!(judge(&branches, None, &[], "main").is_empty(), "with no email of yours, none is yours");
    }

    fn local(branch: &str, pulls: &Pulls, holds: bool) -> Option<Verdict> {
        judge_local(branch, 2, pulls, "main", |_| holds).map(|(v, _)| v)
    }

    #[test]
    fn a_copys_branch_goes_when_its_pull_request_merged_all_of_it_and_never_otherwise() {
        let mine = |state| Pulls { mine: Some(vec![pr(7, "ACME-7", "aaa", state)]), ..Default::default() };
        assert_eq!(local("ACME-7", &mine(PrState::Merged), true), Some(Verdict::Safe));
        assert_eq!(local("ACME-7", &mine(PrState::Merged), false), Some(Verdict::Risky), "commits after what merged");
        assert_eq!(local("ACME-7", &mine(PrState::Open), true), Some(Verdict::Risky), "never pushed");
        assert_eq!(local("ACME-7", &mine(PrState::Closed), true), Some(Verdict::Risky));
        assert_eq!(local("ACME-8", &mine(PrState::Merged), true), Some(Verdict::Risky), "no pull request came from it");
        assert_eq!(local("ACME-7", &Pulls::default(), true), None, "GitHub could not say, so it says nothing");
    }

    #[test]
    fn a_review_branch_goes_when_its_pull_request_holds_every_commit_on_it() {
        let pulls = Pulls {
            reviewed: HashMap::from([(392, Ok((PrState::Merged, "aaa".to_string()))), (441, Err("Not Found".to_string()))]),
            ..Default::default()
        };
        assert_eq!(local("review/customer-portal-392", &pulls, true), Some(Verdict::Safe));
        assert_eq!(local("review/customer-portal-392", &pulls, false), Some(Verdict::Risky));
        assert_eq!(local("review/customer-portal-441", &pulls, true), Some(Verdict::Risky), "GitHub could not say");
        assert_eq!(local("review/customer-portal-500", &pulls, true), None, "never asked about");
        assert_eq!(review_number("review/api-5"), Some(5));
        assert_eq!(review_number("review/notes"), None);
        assert_eq!(review_number("reviewer/api-5"), None);
    }
}
