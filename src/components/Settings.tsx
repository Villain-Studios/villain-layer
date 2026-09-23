import { useEffect, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api } from "../lib/api";
import { useStore } from "../store";
import type { SlackConfig, UiPrefs } from "../lib/types";
import { Field, Modal, Switch } from "./ui";

type Section = "appearance" | "jira" | "github" | "slack" | "general";

/// Pasteable at api.slack.com/apps -> Create New App -> From an app manifest.
const SLACK_MANIFEST = `display_information:
  name: Villain Layer
  description: Agent and pull request notifications from Villain Layer
  background_color: "#0c0c11"
features:
  bot_user:
    display_name: Villain Layer
    always_online: false
oauth_config:
  scopes:
    bot:
      - chat:write
      - chat:write.public
settings:
  org_deploy_enabled: false
  socket_mode_enabled: false
  token_rotation_enabled: false
`;

export function Settings() {
  const settings = useStore((s) => s.settings);
  const agents = useStore((s) => s.agents);
  const toggleSettings = useStore((s) => s.toggleSettings);
  const refreshSettings = useStore((s) => s.refreshSettings);
  const refreshIssues = useStore((s) => s.refreshIssues);
  const toast = useStore((s) => s.toast);
  const fail = useStore((s) => s.fail);

  const [section, setSection] = useState<Section>("appearance");
  const [busy, setBusy] = useState(false);
  const [showManifest, setShowManifest] = useState(false);

  const [jiraUrl, setJiraUrl] = useState("");
  const [jiraEmail, setJiraEmail] = useState("");
  const [jiraToken, setJiraToken] = useState("");
  const [jiraProject, setJiraProject] = useState("");
  const [jiraJql, setJiraJql] = useState("");

  const [ghApi, setGhApi] = useState("https://api.github.com");
  const [ghWeb, setGhWeb] = useState("https://github.com");
  const [ghTeam, setGhTeam] = useState("");
  const [ghToken, setGhToken] = useState("");

  const [slackSecret, setSlackSecret] = useState("");
  const [slackChannel, setSlackChannel] = useState("");

  const [worktreeRoot, setWorktreeRoot] = useState("");

  useEffect(() => {
    if (!settings) return;
    setJiraUrl(settings.jira?.base_url ?? "");
    setJiraEmail(settings.jira?.email ?? "");
    setJiraProject(settings.jira?.project_key ?? "");
    setJiraJql(settings.jira?.jql ?? "");
    setGhApi(settings.github?.api_url ?? "https://api.github.com");
    setGhWeb(settings.github?.web_url ?? "https://github.com");
    setGhTeam(settings.github?.review_team ?? "");
    setSlackChannel(settings.slack?.channel ?? "");
    setWorktreeRoot(settings.worktree_root_is_default ? "" : settings.worktree_root);
  }, [settings]);

  async function connectJira() {
    setBusy(true);
    try {
      const who = await api.jiraConnect(
        jiraUrl.trim(), jiraEmail.trim(), jiraToken.trim(),
        jiraProject.trim() || null, jiraJql.trim() || null,
      );
      setJiraToken("");
      toast("success", `Jira connected as ${who}`);
      await refreshSettings();
      await refreshIssues();
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  async function connectGithub() {
    setBusy(true);
    try {
      const login = await api.githubConnect(
        ghApi.trim(), ghWeb.trim(), ghToken.trim(), ghTeam.trim() || null,
      );
      setGhToken("");
      toast("success", `GitHub connected as ${login}`);
      await refreshSettings();
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  async function connectSlack() {
    setBusy(true);
    try {
      await api.slackConnect(slackSecret.trim(), slackChannel.trim());
      setSlackSecret("");
      toast("success", "Slack connected — check the channel.");
      await refreshSettings();
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  async function disconnect(which: "jira" | "github" | "slack") {
    try {
      await api.disconnect(which);
      await refreshSettings();
      toast("info", `${which} disconnected`);
    } catch (e) {
      fail(e);
    }
  }

  async function saveSlack(prefs: SlackConfig) {
    try {
      await api.setSlackPrefs(prefs);
      await refreshSettings();
    } catch (e) {
      fail(e);
    }
  }

  async function saveUi(ui: UiPrefs) {
    try {
      await api.setUiPrefs(ui);
      await refreshSettings();
    } catch (e) {
      fail(e);
    }
  }

  async function saveWorktreeRoot(path: string) {
    try {
      await api.setWorktreeRoot(path.trim() || null);
      await refreshSettings();
    } catch (e) {
      fail(e);
    }
  }

  async function pickWorktreeRoot() {
    const picked = await openDialog({ directory: true, title: "Task folder location" });
    if (typeof picked === "string") {
      setWorktreeRoot(picked);
      await saveWorktreeRoot(picked);
    }
  }

  return (
    <Modal
      title="Settings"
      wide
      tall
      onClose={() => toggleSettings(false)}
      toolbar={
        <div className="section-tabs">
          {([
            ["appearance", "Appearance"],
            ["jira", "Jira"],
            ["github", "GitHub"],
            ["slack", "Slack"],
            ["general", "General"],
          ] as [Section, string][]).map(([id, label]) => (
            <button
              key={id}
              className={section === id ? "active" : ""}
              onClick={() => setSection(id)}
            >
              {label}
            </button>
          ))}
        </div>
      }
    >

      {section === "appearance" && settings && (
        <>
          <Field
            label={`Interface scale — ${Math.round(settings.ui.scale * 100)}%`}
            hint="Scales everything except terminal text, which has its own size below."
          >
            <input
              type="range"
              min={0.8}
              max={1.6}
              step={0.05}
              value={settings.ui.scale}
              onChange={(e) => void saveUi({ ...settings.ui, scale: Number(e.target.value) })}
            />
            <div className="row" style={{ marginTop: 6 }}>
              {[0.9, 1.0, 1.15, 1.3, 1.45].map((v) => (
                <button
                  key={v}
                  className={`btn btn-sm${
                    Math.abs(settings.ui.scale - v) < 0.001 ? " btn-primary" : ""
                  }`}
                  onClick={() => void saveUi({ ...settings.ui, scale: v })}
                >
                  {Math.round(v * 100)}%
                </button>
              ))}
            </div>
          </Field>

          <Field
            label="Notifications"
            hint="Only while the window is in the background. Opening the app does not announce what was already waiting."
          >
            <div className="switch-list">
              <Switch
                label="New reviews and tickets"
                detail="A banner when a pull request starts waiting on your review, or a ticket is assigned to you."
                checked={settings.ui.system_notifications}
                onChange={(v) => void saveUi({ ...settings.ui, system_notifications: v })}
              />
            </div>
          </Field>

          <Field
            label="Terminals"
            hint="Agents are resumed rather than restarted where their CLI supports it, so the conversation carries on."
          >
            <div className="switch-list">
              <Switch
                label="Put terminals back when the app reopens"
                detail="Panes that were open last time are reopened in the same worktrees."
                checked={settings.ui.restore_panes}
                onChange={(v) => void saveUi({ ...settings.ui, restore_panes: v })}
              />
              <Switch
                label="Let agents read terminal output"
                detail="Agents can read what a terminal here has printed — a dev server's log, a test run — instead of starting a second copy. A shell's scrollback is a record of everything typed in it, so this stays off until you want it."
                checked={settings.ui.agents_read_panes}
                onChange={(v) => void saveUi({ ...settings.ui, agents_read_panes: v })}
              />
              <Switch
                label="Move the ticket when work starts"
                detail="Starting a task transitions its Jira issue into whatever your workflow calls in progress, so the board and this app do not disagree about what is being worked on."
                checked={settings.ui.sync_jira_status}
                onChange={(v) => void saveUi({ ...settings.ui, sync_jira_status: v })}
              />
              <Switch
                label="Trust the folders this app creates"
                detail="Claude Code asks whether it trusts a folder the first time it starts there, and does nothing until answered — once per task, per repo. This answers it in advance, and only for worktrees and chat folders the app made itself."
                checked={settings.ui.trust_agent_dirs}
                onChange={(v) => void saveUi({ ...settings.ui, trust_agent_dirs: v })}
              />
            </div>
          </Field>

          <Field
            label={`Terminal text — ${settings.ui.terminal_font_size}px`}
            hint="Applies to running panes immediately; they re-fit to the new cell size."
          >
            <input
              type="range"
              min={9}
              max={24}
              step={1}
              value={settings.ui.terminal_font_size}
              onChange={(e) =>
                void saveUi({ ...settings.ui, terminal_font_size: Number(e.target.value) })
              }
            />
          </Field>
        </>
      )}

      {section === "jira" && (
        <>
          {settings?.jira_connected && (
            <div className="row" style={{ marginBottom: 14 }}>
              <span className="chip add">connected</span>
              <span className="muted">{settings.jira?.email}</span>
              <div className="spacer" />
              <button className="btn btn-sm btn-danger" onClick={() => void disconnect("jira")}>
                Disconnect
              </button>
            </div>
          )}
          <Field label="Site URL">
            <input
              value={jiraUrl}
              onChange={(e) => setJiraUrl(e.target.value)}
              placeholder="https://your-team.atlassian.net"
            />
          </Field>
          <Field label="Account email">
            <input value={jiraEmail} onChange={(e) => setJiraEmail(e.target.value)} />
          </Field>
          <Field
            label="API token"
            hint={
              <>
                Stored in your macOS keychain, never in config files.{" "}
                <a
                  href="#"
                  style={{ color: "var(--accent)" }}
                  onClick={(e) => {
                    e.preventDefault();
                    void openUrl("https://id.atlassian.com/manage-profile/security/api-tokens");
                  }}
                >
                  Create one ↗
                </a>
              </>
            }
          >
            <input
              type="password"
              value={jiraToken}
              onChange={(e) => setJiraToken(e.target.value)}
              placeholder={settings?.jira_connected ? "•••••••• (leave blank to keep)" : ""}
            />
          </Field>
          <Field label="Project key" hint="Optional. Scopes the default task query.">
            <input
              value={jiraProject}
              onChange={(e) => setJiraProject(e.target.value.toUpperCase())}
              placeholder="ACME"
            />
          </Field>
          <Field label="JQL" hint="Optional. Overrides the default 'assigned to me, not done' query.">
            <input
              value={jiraJql}
              onChange={(e) => setJiraJql(e.target.value)}
              placeholder="assignee = currentUser() AND statusCategory != Done ORDER BY updated DESC"
            />
          </Field>
          <button
            className="btn btn-primary"
            // A blank token keeps the stored one, so once connected the
            // project key and JQL can be changed without pasting it again.
            disabled={
              busy || !jiraUrl.trim() || !jiraEmail.trim() ||
              (!jiraToken.trim() && !settings?.jira_connected)
            }
            onClick={() => void connectJira()}
          >
            {busy ? "Verifying…" : settings?.jira_connected ? "Save & reconnect" : "Connect"}
          </button>
        </>
      )}

      {section === "github" && (
        <>
          {settings?.github_connected && (
            <div className="row" style={{ marginBottom: 14 }}>
              <span className="chip add">connected</span>
              <span className="muted">{settings.github?.api_url}</span>
              <div className="spacer" />
              <button className="btn btn-sm btn-danger" onClick={() => void disconnect("github")}>
                Disconnect
              </button>
            </div>
          )}
          <Field
            label="API URL"
            hint="github.com uses https://api.github.com. Enterprise Server uses https://ghe.example.com/api/v3."
          >
            <input value={ghApi} onChange={(e) => setGhApi(e.target.value)} />
          </Field>
          <Field label="Web URL" hint="Used for building links back to the browser.">
            <input value={ghWeb} onChange={(e) => setGhWeb(e.target.value)} />
          </Field>
          <Field
            label="Review team"
            hint="Optional. A second list of pull requests requested of this team, as @fe or org/fe. A bare name is looked up from the teams you belong to, which needs the read:org scope; org/fe does not."
          >
            <input
              value={ghTeam}
              onChange={(e) => setGhTeam(e.target.value)}
              placeholder="@fe"
            />
          </Field>
          <Field
            label="Personal access token"
            hint="Needs repo scope. Stored in your keychain."
          >
            <input
              type="password"
              value={ghToken}
              onChange={(e) => setGhToken(e.target.value)}
              placeholder={settings?.github_connected ? "•••••••• (leave blank to keep)" : ""}
            />
          </Field>
          <button
            className="btn btn-primary"
            disabled={
              busy || !ghApi.trim() || (!ghToken.trim() && !settings?.github_connected)
            }
            onClick={() => void connectGithub()}
          >
            {busy ? "Verifying…" : settings?.github_connected ? "Save & reconnect" : "Connect"}
          </button>
        </>
      )}

      {section === "slack" && (
        <>
          {settings?.slack_connected && (
            <div className="row" style={{ marginBottom: 14 }}>
              <span className="chip add">connected</span>
              <span className="muted">{settings.slack?.channel}</span>
              <div className="spacer" />
              <button className="btn btn-sm btn-danger" onClick={() => void disconnect("slack")}>
                Disconnect
              </button>
            </div>
          )}
          <div className="card" style={{ marginBottom: 14 }}>
            <h3>Creating the app</h3>
            <div className="muted" style={{ lineHeight: 1.7 }}>
              Slack no longer hands out standalone tokens or webhooks — everything goes
              through an app now. It takes about a minute:
              <ol style={{ margin: "8px 0 0", paddingLeft: 18 }}>
                <li>
                  Open{" "}
                  <a
                    href="#"
                    style={{ color: "var(--accent)" }}
                    onClick={(e) => { e.preventDefault(); void openUrl("https://api.slack.com/apps"); }}
                  >
                    api.slack.com/apps ↗
                  </a>{" "}
                  → <b>Create New App</b> → <b>From an app manifest</b>
                </li>
                <li>Pick your workspace and paste the manifest below</li>
                <li><b>Install to Workspace</b>, then approve</li>
                <li>
                  Copy the <b>Bot User OAuth Token</b> from <b>OAuth &amp; Permissions</b> —
                  it starts with <code>xoxb-</code>
                </li>
              </ol>
            </div>
            <div className="row" style={{ marginTop: 10 }}>
              <button className="btn btn-sm" onClick={() => setShowManifest((v) => !v)}>
                {showManifest ? "Hide manifest" : "Show app manifest"}
              </button>
            </div>
            {showManifest && (
              <textarea
                readOnly
                rows={16}
                value={SLACK_MANIFEST}
                onFocus={(e) => e.currentTarget.select()}
                style={{ marginTop: 10, fontSize: 11 }}
              />
            )}
            <div className="muted" style={{ marginTop: 10, lineHeight: 1.55 }}>
              The manifest asks for <code>chat:write</code> and{" "}
              <code>chat:write.public</code>. The second one is what lets the app post to a
              public channel without being invited first — drop it if you would rather
              invite the bot per channel.
              <br /><br />
              Adding <code>channels:read</code> and <code>channels:history</code> would let
              the app find and delete its own older messages. It is not needed for normal
              use: messages posted from here are remembered and can be deleted without
              them.
            </div>
          </div>

          <Field
            label="Bot token or webhook URL"
            hint="An xoxb- bot token, or an incoming-webhook URL if you already have one."
          >
            <input
              type="password"
              value={slackSecret}
              onChange={(e) => setSlackSecret(e.target.value)}
              placeholder="xoxb-… or https://hooks.slack.com/services/…"
            />
          </Field>
          <Field
            label="Channel"
            hint="Channel name or ID. Ignored for webhooks, which post to their own channel."
          >
            <input
              value={slackChannel}
              onChange={(e) => setSlackChannel(e.target.value)}
              placeholder="#eng-agents"
            />
          </Field>

          {settings?.slack && (
            <Field
              label="What gets posted"
              hint="Muting is enforced in the backend, so an agent cannot route around it."
            >
              <div className="switch-list">
                <Switch
                  label="Send anything to Slack"
                  detail="Master switch. Off means silence, without disconnecting."
                  checked={settings.slack.enabled}
                  onChange={(v) => void saveSlack({ ...settings.slack!, enabled: v })}
                />
                <Switch
                  label="When an agent finishes"
                  checked={settings.slack.notify_on_done}
                  disabled={!settings.slack.enabled}
                  onChange={(v) => void saveSlack({ ...settings.slack!, notify_on_done: v })}
                />
                <Switch
                  label="When pull requests open"
                  checked={settings.slack.notify_on_prs}
                  disabled={!settings.slack.enabled}
                  onChange={(v) => void saveSlack({ ...settings.slack!, notify_on_prs: v })}
                />
                <Switch
                  label="Messages agents send themselves"
                  detail="The slack_post tool. Agents post unattended, so this is separate."
                  checked={settings.slack.allow_agent_posts}
                  disabled={!settings.slack.enabled}
                  onChange={(v) =>
                    void saveSlack({ ...settings.slack!, allow_agent_posts: v })
                  }
                />
              </div>
            </Field>
          )}
          <button
            className="btn btn-primary"
            disabled={busy || !slackSecret.trim()}
            onClick={() => void connectSlack()}
          >
            {busy ? "Posting test message…" : "Connect & test"}
          </button>
        </>
      )}

      {section === "general" && (
        <>
          <Field
            label="Task folder location"
            hint={
              <>
                Each task gets a folder here, holding one worktree per repository
                it touches. Currently <code>{settings?.worktree_root}</code>
                {settings?.worktree_root_is_default ? " (default)" : ""}.
              </>
            }
          >
            <div className="row">
              <input
                value={worktreeRoot}
                placeholder="(default: ~/.villain-worktrees)"
                onChange={(e) => setWorktreeRoot(e.target.value)}
                onBlur={() => void saveWorktreeRoot(worktreeRoot)}
              />
              <button className="btn" onClick={() => void pickWorktreeRoot()}>Browse…</button>
            </div>
          </Field>
          <Field label="Detected agents">
            <div className="col">
              {agents.map((a) => (
                <div key={a.id} className="row" style={{ fontSize: 12 }}>
                  <span className={`dot ${a.installed ? "live" : ""}`} />
                  <span style={{ width: 110 }}>{a.name}</span>
                  <span className="muted" style={{ fontFamily: "var(--mono)", fontSize: 11 }}>
                    {a.path ?? "not found on PATH"}
                  </span>
                </div>
              ))}
            </div>
          </Field>
        </>
      )}
    </Modal>
  );
}
