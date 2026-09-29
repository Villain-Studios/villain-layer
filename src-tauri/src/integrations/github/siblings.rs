//! Each pull request of a multi-repo task names the others (PR-2).
//!
//! Every PR of a task got the same description and nothing else, so whoever
//! reviewed one side could not see from GitHub that there was another: only
//! the Jira comment joined them up. The list lives in a marked section that
//! the app owns and rewrites; the rest of a description is the author's and
//! is left as written.

use serde_json::{json, Value};

use super::GitHub;
use crate::error::Result;

const START: &str = "<!-- villain-layer:siblings -->";
const END: &str = "<!-- /villain-layer:siblings -->";

/// One pull request of the set.
#[derive(Debug, Clone)]
pub struct Sibling {
    /// The repository's name, as the app shows it.
    pub repo: String,
    pub owner: String,
    pub name: String,
    pub number: u64,
    pub url: String,
}

impl GitHub {
    /// Give every PR in `set` a list of the others. Returns the repos whose
    /// PR could not be updated, and why: the PRs are open either way.
    pub async fn link_siblings(&self, set: &[Sibling], ticket: Option<&str>) -> Vec<(String, String)> {
        let mut failed = Vec::new();
        for pr in set {
            let others: Vec<&Sibling> = set.iter().filter(|o| o.url != pr.url).collect();
            if let Err(e) = self.list_others(pr, &others, ticket).await {
                failed.push((pr.repo.clone(), e.to_string()));
            }
        }
        failed
    }

    async fn list_others(&self, pr: &Sibling, others: &[&Sibling], ticket: Option<&str>) -> Result<()> {
        let path = format!("/repos/{}/{}/pulls/{}", pr.owner, pr.name, pr.number);
        let v = self.json(self.req(reqwest::Method::GET, &path)).await?;
        let body = v.get("body").and_then(Value::as_str).unwrap_or_default();
        let next = with_siblings(body, others, ticket);
        if next != body {
            self.json(self.req(reqwest::Method::PATCH, &path).json(&json!({ "body": next })))
                .await?;
        }
        Ok(())
    }
}

/// `body` with its sibling section replaced by one listing `others`, or
/// added at the end when it has none.
pub fn with_siblings(body: &str, others: &[&Sibling], ticket: Option<&str>) -> String {
    let lead = match ticket {
        Some(key) => format!("Part of {key}, together with:"),
        None => "Opened together with:".to_string(),
    };
    let list: Vec<String> = others.iter().map(|o| format!("- {}: {}", o.repo, o.url)).collect();
    let section = format!("{START}\n---\n{lead}\n{}\n{END}", list.join("\n"));

    let own = match body.find(START) {
        Some(at) => {
            let after = body[at..].find(END).map(|e| at + e + END.len()).unwrap_or(body.len());
            let (before, rest) = (body[..at].trim_end(), body[after..].trim_start());
            [before, rest].iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join("\n\n")
        }
        None => body.to_string(),
    };
    let own = own.trim_end();
    if own.is_empty() {
        section
    } else {
        format!("{own}\n\n{section}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(repo: &str, number: u64) -> Sibling {
        Sibling {
            repo: repo.into(),
            owner: "acme".into(),
            name: repo.into(),
            number,
            url: format!("https://github.com/acme/{repo}/pull/{number}"),
        }
    }

    #[test]
    fn a_description_gains_the_other_prs_below_what_was_written() {
        let portal = pr("web", 12);
        let body = with_siblings("Serves the spec.\n", &[&portal], Some("ACME-7"));
        assert_eq!(
            body,
            "Serves the spec.\n\n<!-- villain-layer:siblings -->\n---\nPart of ACME-7, together with:\n\
             - web: https://github.com/acme/web/pull/12\n\
             <!-- /villain-layer:siblings -->"
        );
    }

    #[test]
    fn a_later_repo_replaces_the_list_and_keeps_the_authors_edits() {
        let (portal, iac) = (pr("web", 12), pr("infra", 3));
        let first = with_siblings("Serves the spec.", &[&portal], Some("ACME-7"));
        let edited = first.replace("Serves the spec.", "Serves the spec, and says so.");

        let second = with_siblings(&edited, &[&portal, &iac], Some("ACME-7"));

        assert!(second.starts_with("Serves the spec, and says so.\n\n"));
        assert_eq!(second.matches("villain-layer:siblings -->").count(), 2);
        assert!(second.contains("infra: https://github.com/acme/infra/pull/3"));
        assert_eq!(with_siblings(&second, &[&portal, &iac], Some("ACME-7")), second);
    }

    #[test]
    fn text_written_after_the_list_stays() {
        let portal = pr("web", 12);
        let body = format!("{}\n\nThanks!", with_siblings("Spec.", &[&portal], None));

        let again = with_siblings(&body, &[&portal], None);

        assert!(again.starts_with("Spec.\n\nThanks!\n\n<!-- villain-layer:siblings -->"));
        assert!(again.contains("Opened together with:"));
    }
}
