//! Slack notifications. The stored secret is either a bot token (`xoxb-...`)
//! or an incoming-webhook URL; both are supported so setup can be one paste.

use serde_json::json;

use crate::error::{Error, Result};

pub struct Slack {
    secret: String,
    client: reqwest::Client,
}

impl Slack {
    pub fn new(secret: &str) -> Self {
        Self {
            secret: secret.trim().to_string(),
            client: reqwest::Client::new(),
        }
    }

    fn is_webhook(&self) -> bool {
        self.secret.starts_with("https://hooks.slack.com/")
    }

    pub async fn post(&self, channel: &str, text: &str, context: Option<&str>) -> Result<()> {
        let mut blocks = vec![json!({
            "type": "section",
            "text": { "type": "mrkdwn", "text": text }
        })];
        if let Some(ctx) = context.filter(|c| !c.is_empty()) {
            blocks.push(json!({
                "type": "context",
                "elements": [{ "type": "mrkdwn", "text": ctx }]
            }));
        }

        if self.is_webhook() {
            let res = self
                .client
                .post(&self.secret)
                .json(&json!({ "text": text, "blocks": blocks }))
                .send()
                .await?;
            if !res.status().is_success() {
                return Err(Error::Other(format!("Slack webhook {}", res.status())));
            }
            return Ok(());
        }

        let res = self
            .client
            .post("https://slack.com/api/chat.postMessage")
            .header("Authorization", format!("Bearer {}", self.secret))
            .json(&json!({ "channel": channel, "text": text, "blocks": blocks }))
            .send()
            .await?;

        let body: serde_json::Value = res.json().await?;
        if body.get("ok").and_then(|o| o.as_bool()) != Some(true) {
            let code = body.get("error").and_then(|e| e.as_str()).unwrap_or("unknown");
            return Err(Error::Other(explain(code, channel)));
        }
        Ok(())
    }
}

/// Slack's error codes are terse and the fix is rarely obvious from them.
fn explain(code: &str, channel: &str) -> String {
    match code {
        "not_in_channel" => format!(
            "The bot is not in {channel}. Either invite it (`/invite @your-app` in the channel) \
             or add the chat:write.public scope to the app and reinstall it."
        ),
        "channel_not_found" => format!(
            "Slack cannot see {channel}. Check the name, and note that private channels need the \
             bot invited before it can post."
        ),
        "missing_scope" | "not_allowed_token_type" => {
            "The bot token is missing the chat:write scope. Add it under OAuth & Permissions, \
             then reinstall the app to your workspace."
                .to_string()
        }
        "invalid_auth" | "not_authed" | "token_revoked" | "account_inactive" => {
            "Slack rejected the token. Copy the Bot User OAuth Token (it starts with xoxb-) from \
             OAuth & Permissions, not the app or signing secret."
                .to_string()
        }
        "is_archived" => format!("{channel} is archived."),
        "rate_limited" => "Slack rate-limited the request; try again shortly.".to_string(),
        other => format!("Slack rejected the message: {other}"),
    }
}
