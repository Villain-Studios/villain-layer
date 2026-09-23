//! The statuses a project's tickets can be in, for choosing where they go
//! as the work moves (TKT-8).

use serde::Serialize;
use serde_json::Value;

use super::{segment, str_at, Jira};
use crate::error::Result;

#[derive(Debug, Clone, Serialize)]
pub struct ProjectStatus {
    pub id: String,
    pub name: String,
    /// "new", "indeterminate" or "done".
    pub category: String,
}

impl Jira {
    /// Every status any issue type in `project` can be in, once each, in the
    /// order Jira lists them.
    pub async fn project_statuses(&self, project: &str) -> Result<Vec<ProjectStatus>> {
        let v = self
            .json(self.req(reqwest::Method::GET, &format!("/rest/api/3/project/{}/statuses", segment(project)?)))
            .await?;
        Ok(statuses(&v))
    }
}

/// The answer is per issue type, each listing its statuses: the same
/// status appears under every type that uses it.
fn statuses(v: &Value) -> Vec<ProjectStatus> {
    let mut out: Vec<ProjectStatus> = Vec::new();
    let listed = v.as_array().into_iter().flatten().flat_map(|t| t["statuses"].as_array().into_iter().flatten());
    for s in listed {
        let id = str_at(s, "id");
        if id.is_empty() || out.iter().any(|o| o.id == id) {
            continue;
        }
        let category = s.pointer("/statusCategory/key").and_then(|c| c.as_str()).unwrap_or_default();
        out.push(ProjectStatus { name: str_at(s, "name"), category: category.to_string(), id });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_status_is_listed_once_however_many_types_use_it() {
        let v = serde_json::json!([
            { "name": "Task", "statuses": [
                { "id": "1", "name": "Open", "statusCategory": { "key": "new" } },
                { "id": "3", "name": "In Progress", "statusCategory": { "key": "indeterminate" } },
                { "id": "10322", "name": "Review", "statusCategory": { "key": "indeterminate" } },
            ]},
            { "name": "Bug", "statuses": [
                { "id": "3", "name": "In Progress", "statusCategory": { "key": "indeterminate" } },
                { "id": "10002", "name": "Done", "statusCategory": { "key": "done" } },
            ]},
        ]);
        let got: Vec<(String, String)> = statuses(&v).into_iter().map(|s| (s.name, s.category)).collect();
        assert_eq!(got, [
            ("Open".into(), "new".into()),
            ("In Progress".into(), "indeterminate".into()),
            ("Review".into(), "indeterminate".into()),
            ("Done".into(), "done".into()),
        ]);
    }
}
