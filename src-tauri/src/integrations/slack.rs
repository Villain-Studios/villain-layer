//! Slack notifications. The stored secret is either a bot token (`xoxb-...`)
//! or an incoming-webhook URL; both are supported so setup can be one paste.

use serde_json::json;

use super::http_client;
use crate::error::{Error, Result};

pub struct Slack {
    secret: String,
    client: reqwest::Client,
}

impl Slack {
    pub fn new(secret: &str) -> Self {
        Self {
            secret: secret.trim().to_string(),
            client: http_client(),
        }
    }

    fn is_webhook(&self) -> bool {
        self.secret.starts_with("https://hooks.slack.com/")
    }

    /// Where a posted message landed, so the app can retract it later. Only a
    /// bot can delete a bot's messages, so without this the app leaves litter
    /// nobody else is able to clear up.
    pub async fn post(
        &self,
        channel: &str,
        text: &str,
        context: Option<&str>,
    ) -> Result<Option<Posted>> {
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
                .await
                // A webhook's URL is its secret, and reqwest's errors end
                // "for url (…)": a timeout put the whole thing in a toast, and
                // through `slack_post` in an agent's transcript.
                .map_err(|e| Error::from(e.without_url()))?;
            if !res.status().is_success() {
                return Err(Error::Other(format!("Slack webhook {}", res.status())));
            }
            // Webhooks return no timestamp, so those messages cannot be retracted.
            return Ok(None);
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

        // postMessage resolves a channel name to its id, which is what
        // chat.delete needs later.
        Ok(Some(Posted {
            channel: body
                .get("channel")
                .and_then(|c| c.as_str())
                .unwrap_or(channel)
                .to_string(),
            ts: body
                .get("ts")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string(),
        }))
    }

    /// Delete one of the app's own messages.
    pub async fn delete(&self, channel: &str, ts: &str) -> Result<()> {
        if self.is_webhook() {
            return Err(Error::Other(
                "webhooks cannot delete messages; connect a bot token to do that".into(),
            ));
        }
        let res = self
            .client
            .post("https://slack.com/api/chat.delete")
            .header("Authorization", format!("Bearer {}", self.secret))
            .json(&json!({ "channel": channel, "ts": ts }))
            .send()
            .await?;

        let body: serde_json::Value = res.json().await?;
        if body.get("ok").and_then(|o| o.as_bool()) != Some(true) {
            let code = body.get("error").and_then(|e| e.as_str()).unwrap_or("unknown");
            return Err(Error::Other(match code {
                "message_not_found" => "already gone".to_string(),
                "cant_delete_message" => {
                    "Slack will not let this token delete that message; it can only \
                     delete messages the same bot posted."
                        .to_string()
                }
                other => format!("Slack rejected the delete: {other}"),
            }));
        }
        Ok(())
    }
}

impl Slack {
    /// Which scopes this token actually carries, straight from Slack's own
    /// response header. Saves guessing when a call is refused.
    pub async fn scopes(&self) -> Result<(String, String)> {
        let res = self
            .client
            .post("https://slack.com/api/auth.test")
            .header("Authorization", format!("Bearer {}", self.secret))
            .send()
            .await?;
        let scopes = res
            .headers()
            .get("x-oauth-scopes")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body: serde_json::Value = res.json().await?;
        let bot = body
            .get("bot_id")
            .and_then(|b| b.as_str())
            .unwrap_or_default()
            .to_string();
        Ok((bot, scopes))
    }

    /// Resolve a channel name to its id. Needs channels:read.
    pub async fn channel_id(&self, name: &str) -> Result<String> {
        let wanted = name.trim_start_matches('#');
        let res = self
            .client
            .get("https://slack.com/api/conversations.list")
            .header("Authorization", format!("Bearer {}", self.secret))
            .query(&[("limit", "1000"), ("types", "public_channel,private_channel")])
            .send()
            .await?;
        let body: serde_json::Value = res.json().await?;
        if body.get("ok").and_then(|o| o.as_bool()) != Some(true) {
            return Err(Error::Other(format!(
                "cannot list channels: {}",
                body.get("error").and_then(|e| e.as_str()).unwrap_or("unknown")
            )));
        }
        body.get("channels")
            .and_then(|c| c.as_array())
            .and_then(|a| {
                a.iter()
                    .find(|c| c.get("name").and_then(|n| n.as_str()) == Some(wanted))
                    .and_then(|c| c.get("id").and_then(|i| i.as_str()))
                    .map(str::to_string)
            })
            .ok_or_else(|| Error::Other(format!("no channel named {name}")))
    }

    /// The app's own recent messages in a channel. Needs channels:history.
    pub async fn own_recent(&self, channel: &str, bot_id: &str) -> Result<Vec<Posted>> {
        let res = self
            .client
            .get("https://slack.com/api/conversations.history")
            .header("Authorization", format!("Bearer {}", self.secret))
            .query(&[("channel", channel), ("limit", "200")])
            .send()
            .await?;
        let body: serde_json::Value = res.json().await?;
        if body.get("ok").and_then(|o| o.as_bool()) != Some(true) {
            return Err(Error::Other(format!(
                "cannot read history: {}",
                body.get("error").and_then(|e| e.as_str()).unwrap_or("unknown")
            )));
        }
        Ok(body
            .get("messages")
            .and_then(|m| m.as_array())
            .map(|a| {
                a.iter()
                    .filter(|m| m.get("bot_id").and_then(|b| b.as_str()) == Some(bot_id))
                    .filter_map(|m| {
                        Some(Posted {
                            channel: channel.to_string(),
                            ts: m.get("ts")?.as_str()?.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
}

/// A message the app posted, recorded so it can be taken back.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Posted {
    pub channel: String,
    pub ts: String,
}

/// Pull the channel and timestamp out of a Slack permalink, e.g.
/// https://acme.slack.com/archives/C01ABCD/p1758271234567890
pub fn parse_permalink(link: &str) -> Option<Posted> {
    let (_, rest) = link.split_once("/archives/")?;
    let mut parts = rest.split('/');
    let channel = parts.next()?.to_string();
    let p = parts.next()?;
    let digits: String = p.trim_start_matches('p').chars().take_while(|c| c.is_ascii_digit()).collect();
    if channel.is_empty() || digits.len() < 7 {
        return None;
    }
    // p1758271234567890 -> 1758271234.567890
    let (secs, micros) = digits.split_at(digits.len() - 6);
    Some(Posted {
        channel,
        ts: format!("{secs}.{micros}"),
    })
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
