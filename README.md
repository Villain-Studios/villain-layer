# Villain Layer

A macOS desktop app for working with coding agents: one place to take a
ticket, give an agent a fresh worktree of every repository the ticket
touches, watch which agents need you, and carry the work through review,
pull requests and back to Jira.

It runs the agent CLIs you already use (Claude Code, GitHub Copilot CLI,
OpenCode, Gemini CLI) in real terminals, and connects them to Jira, GitHub
and Slack through the app, so no agent holds your credentials.

**Status: beta.** macOS only. By [Code Villain](https://codevillain.eu/).

## What it does

- **Tasks, not branches.** A task is one ticket across one or more repos:
  one branch name, one folder, a git worktree per repo. Several tasks,
  and several agents per task, run side by side without touching each
  other or your own clones.
- **Start from the ticket.** Pick a Jira ticket and the app suggests the
  repos, creates the worktrees, moves the ticket to in progress, and starts
  an agent with the ticket as its prompt.
- **Know who needs you.** Every agent reports what it is doing: working,
  asking for permission, or done. The window, the dock count and
  notifications say which ones are waiting on you.
- **Review before it leaves.** A diff across all the task's repos, with
  notes on lines you can send straight to the agent. Commit, update from
  the base branch (merge or rebase), push.
- **Pull requests.** Open one PR per repo, see checks and reviews, and hand
  review threads and failing CI logs to the agent in one go. When they
  merge, one step puts the task away: worktrees, branches and the ticket.
- **Tools for the agents.** The app runs an MCP server that its agents use
  to search Jira, file tickets, open PRs, post to Slack and start other
  tasks, with a confirmation for anything irreversible.
- **Chat.** Agents that belong to no task, for questions about your tickets
  and repos.

The full list, with the rules each feature follows, is in
[`docs/features.md`](docs/features.md).

## Requirements

To run it:

- macOS
- git, from the Xcode Command Line Tools (`xcode-select --install`)
- at least one agent CLI on your `PATH`: `claude`, `copilot`, `opencode`
  or `gemini`, already signed in
- optional: a Jira Cloud site, GitHub (or GitHub Enterprise), Slack

To build it, also:

- [Rust](https://rustup.rs) (stable)
- [Bun](https://bun.sh)

## Install

Download the `.dmg` from the
[latest release](https://github.com/Villain-Studios/villain-layer/releases/latest),
open it, and drag Villain Layer to Applications. It runs on Apple silicon
and Intel Macs. To update, quit Villain Layer first: replacing the app
while it runs kills it, and every agent running in it.

A release that is not notarised yet says so in its notes. macOS stops it
the first time with "Apple could not verify…": choose Done, then System
Settings → Privacy & Security → *Open Anyway*.

### Build it yourself

```bash
git clone https://github.com/Villain-Studios/villain-layer.git
cd villain-layer
bun install
bun run release:mac
```

That builds the app and installs it as `/Applications/Villain Layer.app`.
It refuses to run while the app is open, for the same reason. A copy you
built yourself opens without a warning.

## Getting started

1. **Add your repositories.** Repos → *Add repositories* → *Scan a
   folder…* (for example `~/code`), and tick the ones you work in. Group
   them if you like.
2. **Connect what you use** (the gear icon, top right):
   - **Jira**: your site URL, your email, and an API token from
     id.atlassian.com → Security → API tokens.
   - **GitHub**: a token with the `repo` scope. For Enterprise, the API URL
     is `https://<host>/api/v3`. To see your team's review queue, set the
     team as `org/slug`, or as `@slug` with the `read:org` scope.
   - **Slack** (optional): Settings → Slack shows an app manifest. Create a
     Slack app from it, install it, and paste its bot token (`xoxb-…`) and
     a channel. An incoming-webhook URL works too.
3. **Start work.** Tickets → click a ticket → Start work. Check the repos
   and the base branch, pick an agent, Start.
4. **Work with the agent** in the task's Terminals tab. A dot's colour and
   the "N need you" badge say when it is waiting on you.
5. **Review** in the Diff tab: click a line number to leave a note, then
   *Send to agent*. Commit when it is right.
6. **Pull requests** tab: *Open pull request*. When reviews come in,
   *Feedback → agent* hands over the threads and failing checks.
7. **When the PRs have merged**, right-click the task → *Finish task*.

## What it changes on your machine

- **Task folders** go in `~/.villain-worktrees/` (settable). Each holds a
  worktree per repo, plus the app's context files for the agents.
- **The app's own copy of each repo** goes in `~/.villain-worktrees/.repos/`.
  Task worktrees come from it, never from your clone, so you can re-clone,
  move or delete your clones without breaking a task. The copy hard-links
  git's objects, so it takes little extra disk. Task branches show up in
  your own clone once they are pushed and you fetch.
- **`~/.claude.json`**: with *Trust the folders this app creates* on (the
  default), the app marks its own task folders as trusted, so Claude Code
  does not stop to ask. It touches nothing else in that file, and never a
  folder outside the task folder location.
- **Tokens** are kept in the macOS keychain (item
  `eu.codevillain.villain-layer`). They are never written to a file. A
  build without a Developer ID signature (one you built, or a release that
  is not notarised) asks for keychain access again after every update:
  *Always Allow* is tied to the exact binary.
- **Settings and tasks** are in
  `~/Library/Application Support/eu.codevillain.villain-layer/config.json`.

The complete list is in [`docs/features.md`](docs/features.md#14-what-the-app-writes-on-your-machine).

## When something goes wrong

- **The app quit unexpectedly**: the reason is in
  `~/Library/Logs/villain-layer/panic.log`. Please attach it to the report.
- **"MCP server unavailable"** at startup: agents will work, but without
  the app's Jira, GitHub and Slack tools until the next launch.
- **An agent's dot never changes**: that CLI's status reports are not
  arriving. Say which CLI and version.
- **Start over**: quit the app and move `config.json` (above) aside. Your
  repositories and worktrees on disk are not touched.

## Working on it

Start with [`AGENTS.md`](AGENTS.md). It is the rulebook for people and for
coding agents alike, and every agent CLI reads it on its own. Then:

- [`CONTRIBUTING.md`](CONTRIBUTING.md): setup, workflow, pull requests
- [`docs/features.md`](docs/features.md): what the app does, as requirements
- [`docs/architecture.md`](docs/architecture.md): how it works, and why
- [`docs/recipes.md`](docs/recipes.md): how to make the common changes
- [`docs/testing.md`](docs/testing.md): how to verify a change, including
  the UI in a browser
- [`docs/releasing.md`](docs/releasing.md): cutting a release, and signing
  it

## License

Villain Layer is made by [Code Villain](https://codevillain.eu/). You may
use it under either of these licenses, whichever you prefer:

- the Apache License, Version 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE))
- the MIT license ([`LICENSE-MIT`](LICENSE-MIT))

Unless you say otherwise, a contribution you submit for inclusion is
licensed the same way, with no additional terms or conditions.
