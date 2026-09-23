//! Slack connect, notify, cleanup of bot posts.

use serde_json::Value;
use tauri::State;

use crate::config::SlackConfig;
use crate::error::{Error, Result};
use crate::integrations::slack::Slack;
use crate::secrets;

use super::AppState;
use super::diff::RepoResult;

// ------------------------------------------------------------------- slack

/// Remember a posted message so it can be deleted later, newest last.
pub(crate) fn record_post(state: &AppState, posted: Option<crate::integrations::slack::Posted>) {
    let Some(posted) = posted.filter(|p| !p.ts.is_empty()) else {
        return;
    };
    let _ = state.config.update(|c| {
        c.slack_posted.push(posted);
        // Unbounded history would grow the config file forever.
        let len = c.slack_posted.len();
        if len > 100 {
            c.slack_posted.drain(..len - 100);
        }
    });
}

/// What a Slack message is for, so it can be muted on its own.
pub(crate) fn slack_allows(cfg: &SlackConfig, kind: &str) -> bool {
    cfg.enabled
        && match kind {
            "agent_done" => cfg.notify_on_done,
            "prs" => cfg.notify_on_prs,
            "agent_tool" => cfg.allow_agent_posts,
            // Explicit user actions, such as the connection test, are never muted.
            _ => true,
        }
}

/// The client, or None when this kind of message is switched off. Gating lives
/// here rather than in the UI so nothing can route around it.
pub(crate) fn slack_for(state: &AppState, kind: &str) -> Result<Option<(Slack, SlackConfig)>> {
    let (client, cfg) = slack_client(state)?;
    Ok(slack_allows(&cfg, kind).then_some((client, cfg)))
}

pub(crate) fn slack_client(state: &AppState) -> Result<(Slack, SlackConfig)> {
    let cfg = state
        .config
        .read()
        .slack
        .ok_or(Error::NotConfigured("Slack"))?;
    let secret = secrets::get(secrets::SLACK)?.ok_or(Error::NotConfigured("Slack"))?;
    Ok((Slack::new(&secret), cfg))
}

/// Delete messages the app posted. With no `links`, deletes everything it has
/// recorded; otherwise deletes the Slack permalinks given, which is the only
/// way to reach messages posted before the app started recording them.
#[tauri::command]
pub async fn slack_delete_posted(
    state: State<'_, AppState>,
    links: Option<Vec<String>>,
) -> Result<Vec<RepoResult>> {
    let (client, _) = slack_client(&state)?;

    let targets: Vec<crate::integrations::slack::Posted> = match links {
        Some(links) if !links.is_empty() => links
            .iter()
            .filter_map(|l| {
                crate::integrations::slack::parse_permalink(l).or_else(|| {
                    // Bare "channel ts" pairs are accepted too.
                    let (c, t) = l.split_once(char::is_whitespace)?;
                    Some(crate::integrations::slack::Posted {
                        channel: c.trim().to_string(),
                        ts: t.trim().to_string(),
                    })
                })
            })
            .collect(),
        _ => state.config.read().slack_posted,
    };

    if targets.is_empty() {
        return Err(Error::Other(
            "nothing to delete: no recorded messages, and no usable links given".into(),
        ));
    }

    let mut results = Vec::new();
    for t in &targets {
        let (ok, detail) = match client.delete(&t.channel, &t.ts).await {
            Ok(()) => (true, "deleted".to_string()),
            Err(e) => (false, e.to_string()),
        };
        results.push(RepoResult {
            checkout_id: t.ts.clone(),
            repo: t.channel.clone(),
            ok,
            detail,
        });
    }

    // Forget whatever is now gone, so a retry does not report it again.
    let gone: Vec<String> = results
        .iter()
        .filter(|r| r.ok || r.detail == "already gone")
        .map(|r| r.checkout_id.clone())
        .collect();
    state
        .config
        .update(|c| c.slack_posted.retain(|p| !gone.contains(&p.ts)))?;

    Ok(results)
}

/// What the app can see and do in Slack, for when a cleanup is refused.
#[tauri::command]
pub async fn slack_diagnose(state: State<'_, AppState>) -> Result<Value> {
    let (client, cfg) = slack_client(&state)?;
    let (bot_id, scopes) = client.scopes().await?;
    Ok(serde_json::json!({
        "channel": cfg.channel,
        "bot_id": bot_id,
        "scopes": scopes.split(',').map(str::trim).collect::<Vec<_>>(),
        "recorded": state.config.read().slack_posted.len(),
    }))
}

/// Find and delete every message this app posted to its channel, including
/// ones sent before the app started recording them.
#[tauri::command]
pub async fn slack_cleanup(state: State<'_, AppState>, dry_run: bool) -> Result<Value> {
    let (client, cfg) = slack_client(&state)?;
    let (bot_id, scopes) = client.scopes().await?;
    if bot_id.is_empty() {
        return Err(Error::Other(
            "this token is not a bot token, so it has no messages of its own".into(),
        ));
    }

    let channel_id = client.channel_id(&cfg.channel).await.map_err(|e| {
        Error::Other(format!(
            "{e}. The app needs the channels:read scope to find the channel by name \
             (it has: {scopes})"
        ))
    })?;
    let found = client.own_recent(&channel_id, &bot_id).await.map_err(|e| {
        Error::Other(format!(
            "{e}. The app needs the channels:history scope to see its own messages \
             (it has: {scopes})"
        ))
    })?;

    if dry_run {
        return Ok(serde_json::json!({ "would_delete": found.len(), "channel": channel_id }));
    }

    let mut deleted = std::collections::HashSet::new();
    let mut failures = Vec::new();
    for m in &found {
        match client.delete(&m.channel, &m.ts).await {
            Ok(()) => {
                deleted.insert(m.ts.clone());
            }
            Err(e) => failures.push(format!("{}: {e}", m.ts)),
        }
    }
    // Forget only what went. Clearing the whole record also dropped messages
    // whose delete had failed, ones in a channel used before this one, and
    // ones older than the scan reaches — and since only this bot can delete
    // them, nothing could reach them after that.
    state.config.update(|c| {
        c.slack_posted
            .retain(|p| !(p.channel == channel_id && deleted.contains(&p.ts)))
    })?;
    Ok(serde_json::json!({ "deleted": deleted.len(), "failed": failures }))
}

#[tauri::command]
pub async fn slack_connect(
    state: State<'_, AppState>,
    secret: String,
    channel: String,
) -> Result<()> {
    // Reconnecting is how the channel or token gets changed; the switches
    // under it were chosen separately and should not have to be chosen again.
    let cfg = SlackConfig {
        channel: channel.clone(),
        ..state.config.read().slack.unwrap_or_default()
    };
    let posted = Slack::new(&secret)
        .post(&channel, "Villain Layer connected. :white_check_mark:", None)
        .await?;
    secrets::set(secrets::SLACK, &secret)?;
    state.config.update(|c| c.slack = Some(cfg))?;
    record_post(&state, posted);
    Ok(())
}

#[tauri::command]
pub async fn slack_notify(
    state: State<'_, AppState>,
    text: String,
    context: Option<String>,
    kind: Option<String>,
) -> Result<bool> {
    // Muting is not an error: the caller carries on, it just stays quiet.
    let Some((client, cfg)) = slack_for(&state, kind.as_deref().unwrap_or("manual"))? else {
        return Ok(false);
    };
    let posted = client.post(&cfg.channel, &text, context.as_deref()).await?;
    record_post(&state, posted);
    Ok(true)
}

#[tauri::command]
pub fn set_slack_prefs(state: State<AppState>, prefs: SlackConfig) -> Result<()> {
    state.config.update(|c| {
        if let Some(existing) = c.slack.as_mut() {
            // The channel is changed through the connect form, not here.
            existing.enabled = prefs.enabled;
            existing.notify_on_done = prefs.notify_on_done;
            existing.notify_on_prs = prefs.notify_on_prs;
            existing.allow_agent_posts = prefs.allow_agent_posts;
        }
    })
}

