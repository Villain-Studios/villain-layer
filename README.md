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

To produce a distributable app bundle:

```bash
bun run tauri build
```

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

## Layout

Four top-level views:

| | |
|---|---|
| **Work** | your tasks. With one selected: its terminals, diff and pull requests. With none selected: every agent across every task — what it is doing, how long since it last printed, and whether it has gone quiet waiting for you |
| **Tickets** | your Jira backlog, grouped by epic |
| **Chat** | a standing agent with no worktree |
| **Repos** | the repositories Villain Layer knows about, their groups, saved sets and Jira rules |

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
5. **Ship.** One commit message commits every repo that changed. *Push & open
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

Which repos a ticket needs is then answered by a cascade, most confident first:

1. **Jira rules.** "Tickets with component *Payments* touch these repos."
   You write these in Settings → Repos; being explicit, they outrank everything.
2. **The epic.** What the last task under the same epic used. Tickets in one epic
   usually hit the same repos.
3. **The Jira project.** What the last task in the project used.

Whatever it picks, the picker says *why* — "preselected from last task under
ACME-21131" — so a stale guess is visible rather than silent, and you can always
change it.

**Saved sets** sit alongside that: select some repos, hit *save these as a set*,
name it, and it becomes a one-click chip in the picker from then on. Good for
combinations that recur but that no rule quite describes.

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
| **Slack** | An app with a bot token — see below | A message when an agent finishes and when a task's PRs open |

The default Jira query is `assignee = currentUser() AND statusCategory != Done`,
scoped to your project key if you set one. Override it with your own JQL in
Settings.

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
                    the frontend so multi-byte sequences never split
  shellenv.rs       asks your login shell for its real PATH — a GUI app launched
                    from Finder cannot see ~/.local/bin otherwise
  agents.rs         the agent catalogue and how each one takes an opening prompt
  commands.rs       the Tauri command surface; most commands take a task id and
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
