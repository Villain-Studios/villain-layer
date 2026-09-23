# Villain Layer

An agent development environment: run coding agents in parallel, each isolated in
its own git worktree, with real terminals, diff review, Jira, GitHub Enterprise
and Slack wired in.

Tauri 2 (Rust) + React 19. macOS, Windows and Linux.

## Getting started

```bash
bun install
bun run tauri dev
```

That shares a config with an installed build. If you keep one for daily use, run
the dev build under its own identity instead — see below.

To produce a distributable app bundle:

```bash
bun run tauri build
```

On macOS, to build one and install it over the copy in `/Applications`:

```bash
bun run release:mac
```

It refuses while the app is running, before building anything. Replacing a
bundle under a live process kills it, and takes its agents with it.

### Two instances at once

Developing the app while using it means two copies running: an installed release
build doing the real work, and a dev build changing under you. They cannot share
state. The config is read once at launch and written back whole, so two
instances pointed at one file would take turns erasing each other's tasks.

The dev build therefore runs under its own identity:

```bash
bun run dev:app
```

That merges `src-tauri/tauri.dev.conf.json` over the normal config, giving the
dev build the identifier `dev.villain.layer.dev` and with it a config directory
of its own. Point its worktree root somewhere separate too — Settings, or
`worktree_root` in that config — so dev task folders do not land among real
ones. The keychain item is shared, so the dev build inherits the Jira, GitHub
and Slack tokens without asking again.

One dev build at a time: Vite holds port 1420 with `strictPort`, so a second
`bun run dev:app` fails loudly instead of running against a server that belongs
to the other one.

## The model: one task, many repos

A **task** is one unit of work — usually one Jira ticket. It owns one
**checkout** per repository the change touches, all on the same branch, laid out
as sibling folders under one task directory:

```
~/.villain-worktrees/ACME-123-fix-login-race/
  api/          worktree of api,        branch acme-123-fix-login-race
  web/          worktree of web,        branch acme-123-fix-login-race
  shared-lib/   worktree of shared-lib, branch acme-123-fix-login-race
```

That layout is the point. An agent started at the **task root** sees every repo
at once, so it can change an API contract and its consumer in a single session —
which is usually why a ticket spans repos in the first place. Cross-repo tickets
are cross-repo because the changes are coupled.

One branch name across every repo also means sibling PRs are findable by branch
alone, with no extra bookkeeping.

### Branch names

A ticket's branch is its key: `ACME-21215`. If you want something more descriptive,
the Start work dialog takes an optional suffix, giving `ACME-21215-locale-switch` —
typed however you like, slugified on the way in. The task folder is named after
the branch, so the two are always findable from each other.

Tasks created without a ticket get `villain/<slug-of-the-name>`, and the New task
dialog takes a fully explicit branch name if you would rather set one yourself.

## Layout

Five top-level views:

| | |
|---|---|
| **Work** | your tasks. With one selected: its terminals, diff and pull requests. With none selected: every agent across every task — what it is doing, how long since it last printed, and whether it has gone quiet waiting for you |
| **Tickets** | your Jira backlog, grouped by epic, with each issue's own Jira type icon |
| **Reviews** | pull requests waiting on your review, and — if a review team is set — on that team's |
| **Chat** | a standing agent with no worktree |
| **Repos** | the repositories Villain Layer knows about, and their groups |

Work flows Tickets → Work: an issue becomes a task.

A task whose agent has printed nothing for 45 seconds is marked *idle — may need
you*, in the sidebar and in the overview. That is usually a permission prompt
waiting for an answer.

## The loop

1. **Add repos.** `+` on the Repositories section. *Scan a folder* points at a
   parent directory — `~/code/backend`, say — and finds every git repo up to three
   levels down, so a whole stack goes in at once, landing in a group named after
   the folder you scanned; *Pick folders* selects specific ones (⌘-click for
   several). Repos must already be cloned locally.
2. **Start a task.** Either `+` on the Tasks section, or open a ticket in the
   Tickets tab and press *Start work* — pick the repositories it touches, and you
   get a worktree in each plus an agent primed with the ticket *and* the folder
   layout.
3. **Watch it work.** Panes run either at the task root or inside one repo; the
   `all / api / web` tags in the pane bar pick the scope for whatever you launch
   next. Shells sit alongside agents for dev servers and test watchers.
4. **Review.** The Diff tab shows every change across every repo, grouped by
   repo. Click a line number to leave a note; *Send to agent* batches them into
   one prompt. Notes are path-qualified with their repo when the target agent is
   sitting at the task root, so `api/src/auth.ts:42` is never ambiguous.
5. **Ship.** *Draft with agent* on the Pull requests tab asks the agent that did
   the work to write the description — it knows what the diff cannot say: what it
   tried, what it left unfinished, where review effort is best spent. One commit
   message commits every repo that changed. *Push & open
   PRs* pushes each and opens one PR per repo — then posts all the links back to
   the Jira ticket as a single comment, and one summary to Slack.

Repos join and leave a task at any time: expand a task in the sidebar for
`+ add repo`, or the `✕` on a repo row to drop it. You rarely know the full
blast radius when you start.

## Organising a dozen repositories

Past about ten repos a flat list stops being usable, so repos carry a **group**
— `frontend`, `backend`, whatever you like — and one repo belongs to exactly one
group. Groups are collapsible in the sidebar and in the picker, and the picker
gets a filter box and select-all per group.

Which repos a ticket needs is then answered from what you did last, most
confident first:

1. **The epic.** What the last task under the same epic used. Tickets in one epic
   usually hit the same repos.
2. **The Jira project.** What the last task in the project used.

Whatever it picks, the picker says *why* — "preselected from last task under
ACME-21131" — so a stale guess is visible rather than silent, and you can always
change it.

## Chat

A standing agent with no worktree, for the work that happens before a task
exists: asking a question, drafting a ticket, pulling context together. It runs
in a scratch folder under the worktree root, so it can keep notes between
sessions, and it starts with your own CLI configuration — so whatever MCP servers
you have set up for Claude Code or Codex are available here too. If your agent
can reach Jira and Slack, you can ask it to read a thread and file a ticket
without leaving the app.

It is deliberately not a bespoke chat UI: it is the same agent CLI you already
pay for, in the same terminal, so there is no second API key and no second set of
tool integrations to maintain.

## When an agent runs out, or the app restarts

**Handing off.** No agent CLI can resume another's session — Claude Code,
Codex, OpenCode and Cursor each keep their own transcript format, and
translating between them would break on every release. So the ⇄ on an agent
pane moves the *work* rather than the conversation: the ticket, the commits and
diff so far, and the tail of the outgoing agent's terminal, stripped of ANSI and
de-duplicated. That last part matters — running out of budget is exactly when an
agent cannot summarise itself.

The app also watches pane output for the CLIs' own limit messages. When one
appears the pane is flagged, the Work overview marks it, and a banner offers the
handoff. It is best-effort pattern matching, deliberately specific enough not to
fire when an agent merely reads code about rate limiting.

Stopping an agent, closing a pane and quitting the app all signal the process
group with SIGTERM and wait before insisting. This is not politeness for its own
sake: `ChildKiller::kill` is SIGKILL, which an agent cannot catch, so it dies
without writing the transcript that makes the session resumable at all. Quitting
was worse still — with no exit handler the agent got SIGHUP from the closing
terminal and went the same way.

**Resuming.** Panes do not survive the app closing — they are processes, not
records. But the CLIs keep transcripts per working directory, and every task has
its own, so the conversation can be picked up even though the process is gone.
Panes that were open when the app closed are reopened when it starts, resuming
each agent's conversation where its CLI can — turn it off in Settings →
Appearance. Where a saved conversation exists but nothing is running, Terminals
also offers *⟲ Resume Claude Code* with how long ago it was last active. The app reads each CLI's own store rather than
keeping its own list, so it is right about sessions it never started.

Only Claude Code supports this today (`--continue`); the others are listed
without it rather than claiming support that has not been verified.

**A new worktree is an untrusted folder.** Claude Code asks "do you trust the
files in this folder?" the first time it runs anywhere new, and every task is
somewhere new — so it asks once per task, and until it is answered the agent has
not started, written a transcript, or read the prompt it was given. That looks
exactly like an agent silently doing nothing, so the app watches for the question
and says so in the pane and in the Work overview.

## Agents can drive the app

Villain Layer hosts its own MCP server, so an agent it launches reaches Jira,
GitHub and Slack through the credentials the app already holds — no second
login, and no copy of your tokens in the agent's environment. It also sees live
app state and can act on it.

| | |
|---|---|
| Read | `list_tasks`, `list_repos`, `task_diff`, `jira_search`, `jira_get_issue` |
| Read | `task_prs`, `slack_diagnose` |
| Write | `jira_create_issue`, `jira_comment`, `jira_transition`, `slack_post`, `slack_delete`, `slack_cleanup`, `create_task`, `start_work`, `open_prs` |

Slack messages the app posts are recorded, because only a bot can delete a
bot's messages — without that they are litter nobody can clear. `slack_delete`
takes permalinks and needs only `chat:write`; `slack_cleanup` finds them itself
but needs `channels:read` and `channels:history`, and says so when they are
missing.

The server binds an ephemeral port on loopback and is gated on a bearer token
minted fresh each run. Most writes are **not** confirmed — an agent can file a
ticket or post to Slack unattended, which is the point. A smaller set of
high-impact tools (`open_prs`, applying a `jira_transition`, `forget_repo`,
`slack_delete`, and a non-dry-run `slack_cleanup`) refuse unless the call also
passes `confirm: true`, so the agent has to come back after asking.

Agents get the config automatically:

- The chat folder holds a `.mcp.json`, written at start-up rather than only when
  the app launches an agent — so running `claude` there yourself gets the same
  tools and the same context file.
- Task agents get it too. Where the agent's working directory is a git worktree,
  the config lives in the task folder and is passed with `--mcp-config`, because
  a generated `.mcp.json` inside a worktree would show up as an untracked change
  and could be committed by accident. Agents without such a flag get the tools
  only when they run at the task root, which is where multi-repo tasks start
  them anyway.

## Appearance

Settings → Appearance has two independent controls:

- **Interface scale** (80–160%) — scales the whole chrome at once
- **Terminal text** (9–24px) — applies to running panes immediately

They are separate on purpose. xterm measures a cell grid to lay out the terminal,
and a page zoom would leave that calculation fractionally out, so terminal text
scales on its own rather than riding along with the chrome.

### Why the ticket is the hub

Sibling PRs are linked *through the Jira ticket*, not to each other. Cross-linking
N PRs to each other needs a second write pass over every PR body once they all
exist, and it rots the moment you add another repo. The ticket is already the
shared parent, it is already where non-engineers look, and one comment covers it.

## Integrations

Tokens are verified before they are saved, and they live in the macOS keychain —
never in `config.json`.

| | Setup | What it gives you |
|---|---|---|
| **Jira** | Site URL, account email, [API token](https://id.atlassian.com/manage-profile/security/api-tokens) | Your assigned issues, ticket → worktrees + primed agent, status transitions, and the PR-set comment |
| **GitHub** | API URL, web URL, PAT with `repo` scope | Per-repo PR state and check runs, one-action PR opening across the task. Point `api_url` at `https://ghe.example.com/api/v3` for Enterprise Server |
| **Slack** | An app with a bot token — see below | A message when an agent finishes and when a task's PRs open, each switchable |

The default Jira query is `assignee = currentUser() AND statusCategory != Done`,
scoped to your project key if you set one. Override it with your own JQL in
Settings.

### Issue types

Type badges come from the Jira site, not from a table in this repo: the backend
reads `/rest/api/3/issuetype` and inlines each icon as a data URI (the avatar
URLs need authentication, so an `<img>` tag could not fetch them itself). A
project with custom types renders exactly as it does in Jira.

The only thing assumed is `hierarchyLevel`, which is structural in Jira itself —
1 and above is epic-level, 0 a standard issue, -1 a sub-task. That drives the
card's left accent, so epics stand out even before you read the icon. Types
whose icon cannot be fetched fall back to a generated badge keyed to the same
hierarchy rather than vanishing.

### Where tokens live

All three tokens share **one** keychain item, read at most once per launch and
cached for the life of the process. Connection status comes from the config file
rather than the keychain, since a config is only written after its token
verified — so starting the app touches the keychain zero times, and nothing
prompts until something actually calls an API.

macOS ties "Always Allow" to the exact binary that created the item, so a dev
build re-prompts after every recompile. A signed release build (`bun run tauri
build`) keeps the grant.

### Slack

Slack no longer issues standalone tokens or webhooks — everything goes through an
app. Settings → Slack has the manifest and the steps in-app, but in short: create
an app at [api.slack.com/apps](https://api.slack.com/apps) *from an app manifest*,
paste the manifest the app shows you, install it to the workspace, and copy the
**Bot User OAuth Token** (`xoxb-…`) from OAuth & Permissions.

The manifest requests `chat:write` and `chat:write.public`. The second is what
lets the bot post to a public channel it has not been invited to; drop it if you
would rather `/invite` the bot per channel. An existing incoming-webhook URL also
works — paste it in place of the token and the channel field is ignored.

Not everything belongs in Slack, so each kind of message has its own switch in
Settings → Slack: agents finishing, pull requests opening, and messages agents
send themselves through the `slack_post` tool — plus a master switch that mutes
the lot without disconnecting. The gate is enforced in the backend, so an agent
cannot route around a setting by calling the tool directly.

Slack's error codes are terse, so they get translated: `not_in_channel` tells you
to invite the bot or add the scope, `invalid_auth` reminds you the token starts
with `xoxb-`, and so on.

## How it works

```
src-tauri/src/
  config.rs         tasks, checkouts and projects, persisted atomically, with
                    migration from the v1 one-repo-per-workspace shape
  git.rs            worktree lifecycle, status, diff — the git CLI, not libgit2,
                    so hooks and credential helpers keep working
  pty.rs            one PTY per pane via portable-pty; output is base64-framed to
                    the frontend so multi-byte sequences never split, and counted
                    by position, so a terminal back on screen gets only what it
                    missed
  shellenv.rs       asks your login shell for its real PATH — a GUI app launched
                    from Finder cannot see ~/.local/bin otherwise
  agents.rs         the agent catalogue and how each one takes an opening prompt
  commands/         the Tauri command surface; most commands take a task id and
                    fan out over its checkouts
  secrets.rs        keychain access
  integrations/     jira.rs, github.rs, slack.rs
```

Task folders live under `~/.villain-worktrees` by default; change the location in
Settings → General. Tasks created by an older version keep their existing
worktree paths — each checkout records its own path, so both layouts coexist.

Agent panes keep ~256 KB of scrollback in the backend, so switching tabs or
tasks never loses what an agent printed.

### Agent scope

| Task | Default cwd | Why |
|---|---|---|
| One repo | inside the worktree | the agent keeps its normal git awareness |
| Several repos | the task root | it can read and edit across all of them |

Either is available from the launch modal or the pane-bar scope tags. At the task
root the cwd is not itself a git repo, so agents' built-in git features degrade —
that is the trade for cross-repo visibility, and why single-repo tasks do not pay
it.

### Adding an agent CLI

Any interactive CLI works. Add an entry to `AGENTS` in
[`src-tauri/src/agents.rs`](src-tauri/src/agents.rs) with its program name and
how it takes an opening prompt:

- `PromptMode::Positional` — `agent "do the thing"`
- `PromptMode::Flag("-i")` — `agent -i "do the thing"`
- `PromptMode::Typed` — no prompt argument; it gets typed into the TUI after start-up

Agents not found on your PATH are shown greyed out in Settings → General.

## Tests

```bash
cd src-tauri && cargo test
```

Covers the git layer against real throwaway repositories (worktree lifecycle,
one branch across several repos, change detection across
committed/uncommitted/untracked files, remote URL parsing) and the v1 → v2 config
migration, including that it is idempotent.
