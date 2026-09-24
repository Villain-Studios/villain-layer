# Architecture

How Villain Layer is put together, why it is shaped this way, and what has
gone wrong in each part before. Read the section for the part you are about
to change. What the app *does* is in [`features.md`](features.md).

## The shape

```
 ┌───────────────────────── webview (React 19, zustand) ─────────────────────────┐
 │  views & dialogs ── store.ts (all state, refreshes) ── lib/api.ts (IPC only)  │
 │  Terminal.tsx: one pooled xterm per pane            ▲ events    │ invoke      │
 └──────────────────────────────────────────────────────┼───────────┼────────────┘
                                                        │           ▼
 ┌──────────────────────────── Rust (Tauri 2) ──────────┴─────────────────────────┐
 │ commands/*  ── every command the UI can call; thin orchestration               │
 │   ├─ config.rs    AppConfig → config.json (whole-file writes)                  │
 │   ├─ git.rs       the only place git runs                                      │
 │   ├─ pty.rs       panes: PTYs, output buffer, stop/kill, activity state        │
 │   ├─ agents.rs    agent CLI catalogue, per-CLI launch wiring                   │
 │   ├─ integrations/ Jira, GitHub, Slack HTTP clients     secrets.rs → keychain  │
 │ mcp.rs     MCP server on 127.0.0.1 for agents; /hook/<pane> for their states   │
 │ attention.rs, news.rs   background watchers: dock count, banners               │
 └───────────────┬───────────────────────────────────────────────┬──────────────┘
                 │ PTY                                             │ HTTPS
        agent CLIs, shells ── MCP over HTTP ──▶ mcp.rs       Jira · GitHub · Slack
        (in task folders)  ── curl ─────────▶ /hook
```

A task is a folder holding one worktree per repo. The worktrees belong to
the app's own copy of each repository, not to the user's clone:

```
~/.villain-worktrees/
  .repos/api.git, .repos/web.git    ← the app's bare copies (hard-linked from ~/code/api, …)
  ACME-123/                         ← task folder; agents start here
    AGENTS.md, CLAUDE.md            ← task context, written by the app
    .mcp.json                       ← the app's MCP server (0600)
    api/                            ← worktree of .repos/api.git, branch ACME-123
    web/                            ← worktree of .repos/web.git, branch ACME-123
```

## Where work runs

| Thread | What runs there | Rule |
|---|---|---|
| main | plain `#[tauri::command] fn`, the event loop, AppKit | Nothing that waits. While it waits, no event reaches the webview. |
| tokio workers | `async fn` commands, HTTP clients, the MCP server | Nothing that blocks. The MCP server shares these. |
| blocking pool | `commands::blocking(app, \|state\| …)`, `commands::off_runtime(\|\| …)` | git, file walks, child processes, the keychain |
| per pane | a reader, a writer and a waiter thread, plus flush timers | owned by `pty.rs` |
| background | `attention` (5s), `news` (3 min while away), restore and login-shell warm-up at startup | started in `lib.rs` |

`commands::blocking` exists because `delete_task` once froze the whole app
for fifteen seconds, stopping three agents on the main thread. The guard's
`main-thread` and `runtime-blocking` rules catch the direct cases. A helper
that shells out, called from a command, still counts: check what you call.

## State

**Persistent: `config.json`** (`config.rs`), in
`~/Library/Application Support/<identifier>/`. One `AppConfig`: repos,
tasks, checkouts, saved panes, UI prefs, integration settings (never
tokens), and a schema `version`.

- Read once at launch. `config.read()` returns a clone.
- Changed only through `config.update(|c| …)`. That takes a disk lock,
  applies the change under the write lock, then writes the whole file to a
  temp file, fsyncs and renames. **Do not call any `ConfigStore` accessor
  inside the closure**: the locks are not reentrant and it deadlocks.
- Every field has `#[serde(default)]`. A field without one made a config
  missing that key unreadable, and the app started with no tasks.
- `AppConfig` does not deny unknown fields. Removing a field silently drops
  that key on the next write. That is usually what you want, unless it held
  something.
- A newer schema than the build knows is copied to `config.json.v<N>` and
  never migrated down. A file that will not parse is kept as
  `config.json.unreadable`.

**Secrets: the keychain** (`secrets.rs`). One item per build identifier
holds every token as JSON. It is one item because macOS prompts per item
per process: three items meant a stack of password dialogs on every launch.
It is read once per process and cached, and written under a lock.

**In memory: `AppState`** (`commands/mod.rs`) holds the config store, the
PTY manager, per-run caches (Jira issue types, the Epic Link field
lookup, git status per checkout), notices queued before the UI listened,
and what news has been seen.

**Frontend: `store.ts`.** One zustand store holds every list the UI shows,
the refresh actions and the derived helpers. Layout that should survive a
restart (selected task, view, tab, sidebar) goes to `localStorage` through
`lib/persist.ts`, which never throws.

## The IPC contract

- **Commands.** A `#[tauri::command]` in `commands/<area>.rs`, registered
  in `generate_handler!` in `lib.rs`, wrapped once in `src/lib/api.ts`.
  Tauri renames parameters, so `task_id` in Rust is `taskId` in the invoke.
  A struct parameter (`req: NewTask`) keeps its fields snake_case. The guard
  checks names and argument keys on both sides.
- **Types.** Return types are `Serialize` structs, mirrored by hand in
  `src/lib/types.ts` with the same snake_case field names. Change both
  together. The mock harness's fixtures are typed against `types.ts`, so a
  change there breaks `tsc` until the fixtures follow.
- **Errors.** `error::Error` serialises as its message. The UI shows it
  with `errMessage` and `toast`. A command over several repos returns one
  row per repo instead (`RepoResult`, a row `error`) and shows them with
  `reportRepoResults`.

## Events

Backend → frontend. The guard checks that every event emitted is listened
for, and the reverse, and that each is in this table.

| Event | Payload | Emitted when | Listened in |
|---|---|---|---|
| `pty:output` | `{ pane_id, data: base64, end }` | a watched pane printed (batched, ~33ms) | `lib/ptyOutput.ts`, one listener for all panes |
| `pty:activity` | pane id | a pane's state changed: a hook report, a keystroke, a notice cleared, a pane shown | `Watchers.tsx` → `refreshPanes` |
| `pty:notice` | `{ pane_id, notice }` | a usage-limit or trust prompt appeared | `Watchers.tsx` → refresh and toast |
| `pty:exit` | `{ pane_id, code }` | a pane's process ended | `Watchers.tsx` → refresh, toast, Slack |
| `pr:draft` | `{ task_id, text }` | a chunk of a drafted PR description | `PrPanel.tsx` |
| `issue:draft` | `{ request_id, text }` | a chunk of an improved ticket description | `tickets/OptimizeDescription.tsx` |
| `system-notify-click` | a `Target` (`target.rs`) | a banner was clicked | `Watchers.tsx` → `goTo`, which opens what it is about (NOTE-4) |
| `app:notices` | none | a notice was queued after startup (`commands::notify`) | `Watchers.tsx` → `takeNotices`, as toasts |

What still polls, and why, is marked at each `setInterval` with
`// guard: allow poll — <reason>`. Everything polls only while the window is
in front, and slower once nobody has touched it for 45 seconds
(`Watchers.tsx`).

## Panes

`pty.rs` owns every pane. `commands/panes.rs` decides what to start where.

- **Spawning.** The cwd must exist (PANE-3). The environment is cleared,
  then filled from the login shell's (`shellenv.rs`, scraped once behind a
  marker, with a timeout), then `VILLAIN_PANE`. The pane cap is checked
  under the map lock, counting spawns in flight.
- **Output.** Each pane keeps 256KB of scrollback with a running byte
  position. It travels base64-encoded, so a multi-byte character split
  across two chunks is never mangled. A pane that nothing shows is not
  sent anything. When it comes on screen, `pty_attach(since)` hands over
  exactly what came after `since`, or says `reset` when that is gone.
  While watched, output is batched to ~33ms. A keystroke's echo goes out
  within 4ms.
- **The frontend's side** (`Terminal.tsx`): one xterm per pane, created
  once and moved into whichever view shows it. Unmounting parks it and
  hiding detaches it. It is disposed when the pane leaves the store's
  list. At most six hold a WebGL context. After every fit, the size is
  pushed with `pty_resize`, because xterm only reports a change of its own
  grid.
- **Stopping.** `request_stop` sends SIGTERM to an agent's process group
  (SIGHUP to a shell's). `stop` waits out a grace period, then
  `force_kill` sends SIGKILL to the group. `close_matching` signals every
  pane of a task at once and waits once. The map lock is never held while
  waiting.
- **Activity state** (`PaneMeta::state`), in order: a shell is idle; an
  exited pane is idle; a notice makes it asking; otherwise the agent's own
  last report; otherwise output timing, for a CLI that reports nothing.
  Reports arrive at `/hook/<pane>` and are applied under one lock by
  `report_with`. Keystrokes adjust it too (`typed`): Esc at a question is a
  refusal.

## Agents

`agents.rs` has the catalogue: `AGENTS`, one `AgentDef` per CLI. Each CLI
has an `Integration` variant saying how it reports its state and how it
gets the MCP server. `prepare_launch` writes that CLI's files into the
app's config folder and returns its extra arguments and environment.
`commands/panes.rs` (`plug_in`) adds the hook URL and tokens.

- **Hooks** (Claude, Copilot) are shell commands the CLI runs on events.
  Each posts `{"hook_event_name": …}` with curl and the hook token. It
  drains stdin: a PostToolUse payload can be megabytes. It always exits 0,
  so a down server never fails the agent's turn.
- **OpenCode** loads a plugin that posts its state. **Gemini** is read from
  the mark it puts in its window title, parsed from the output.
- **`hook_activity`** turns a post into a state, chosen by what the *pane*
  runs, never by anything in the post.
- **The MCP server** (`mcp.rs`) is axum on `127.0.0.1:0` with a fresh
  bearer token per run. It speaks JSON-RPC over `POST /mcp`. Tools are
  defined in `tools()` and handled in `call()`. Those in `DANGER` need
  `confirm: true`. The endpoint's `hook_token` is separate and accepted
  only by `/hook`.
- **Context files.** `write_task_context` writes the same text as
  `AGENTS.md` and `CLAUDE.md` into a task folder: the ticket, the layout,
  the tools. Chat rooms get their own version. Nothing goes into a
  worktree (DISK-1).

## Git

`git.rs` (and `git/store.rs`, its submodule) is the only place git runs,
through a private `run`. Every call
sets `GIT_OPTIONAL_LOCKS=0` (the status poll must not take `index.lock`
while an agent commits) and `GIT_TERMINAL_PROMPT=0`. It also uses the login
shell's PATH, so hooks find `node`.

The user's own git config applies to every git the app runs, so anything
the app relies on is spelled out:

| Their config | What broke | What we pass |
|---|---|---|
| `merge.ff = only` | every Update from base: "Not possible to fast-forward" | `merge --ff` |
| `status.showUntrackedFiles = no` | a worktree of only new files read as clean, and was removed with them | `--untracked-files=normal`, `-c status.showUntrackedFiles=normal` |
| `diff.external` (difftastic) | the Diff view showed no hunks | `--no-ext-diff` |
| `rebase.autoStash`, `autoSquash`, `updateRefs` | rebases that did more than asked | `-c rebase.*=false` |
| a `--single-branch` clone | fetches that updated nothing | explicit refspecs |

Use `-c` for what has no flag. An older git ignores a config key it does
not know, but refuses an option it does not know.

Anything from outside that reaches a git argument is checked:
`check_names`, `commit_id`, the leading-`-` guards, `--` before paths. A
base of `--upload-pack=<cmd>` once ran the command.

**The app's own copies** (`git/store.rs`). Adding a repo makes
`<task folder location>/.repos/<name>.git` with `git clone --bare --local`.
That hard-links the objects, so it costs little disk, and the links
outlive the user's clone. The copy fetches from the clone's `origin`,
keeps reflogs, and takes the clone's repo-local settings (identity,
signing, `sshCommand`, hooks path) once, when it is made. At every launch,
before panes come back, `adopt_worktrees` moves any worktree still
registered in a user's clone onto the copy, in place. The branch comes
across first, and the index is copied so staging survives. A folder whose
link is already gone is linked back at `Checkout.last_head`, which the
status poll keeps current. A folder cloned again in place is linked back
too (`reclaim_clone`), but only when that loses nothing: its branch must
fast-forward the copy's, and it must hold nothing the copy would not keep. Per-worktree operations (remove, prune, the
branch deleted at finish) ask the worktree which repository owns it
(`owner_of`), so a worktree that could not be moved is still handled.

**Keeping them in shape** (`git/upkeep.rs`, `commands/repos.rs`,
`commands/cleanup.rs`). Whether a folder is "the same repository" is
decided one way everywhere: it fetches from where the copy does,
`same_remote` treating ssh and https spellings as one (`is_store_of`).
Sync fetches origin into the copy with `--prune`, carries the clone's
settings again, and fast-forwards the clone's default branch: its
`origin/<branch>` is written from the copy, so there is no second trip to
the network, and the branch moves by `merge --ff-only` where it is
checked out or by a compare-and-swap `update-ref` where it is not. Clean
up lists before it removes, and removes by id from a list made again at
that moment, so nothing is taken on the strength of an old look. What the
other build uses is read from its `config.json` beside this build's
(`ConfigStore::other_builds`), since both default to the same task folder
location.

**Branch point and lease.** `Checkout.base_commit` is where the branch was
cut. It moves to what was merged in or rebased onto. `git::baseline` uses
it, and falls back to the merge base when HEAD never reached it (a merge
abandoned half way). `Checkout.push_lease` is the remote tip at the last
rebase. `git::push` sends `--force-with-lease=<branch>:<lease>`, drops the
lease if the branch still contains it, and the caller clears it after a
push.

## Integrations

- **One HTTP client** (`integrations::http_client`), cloned: a 30s timeout,
  a 10s connect timeout, a user agent and a shared connection pool. A new
  client per command paid a TLS handshake every time. A call that can take
  longer (job logs) sets its own timeout.
- **Tokens** come from `secrets.rs` per call. `stored_or` decides whether a
  blank token field may reuse the saved one (same origin only).
- **Jira** is REST v3 with Basic auth. JQL strings go through `jql_string`,
  path segments through `segment()`. Search pages by `nextPageToken`, and
  `createmeta` pages too. Names are never assumed (TKT-2). Where a ticket
  goes as its PRs move (TKT-8) is a status id chosen per project, since the
  categories cannot tell Review from In Progress; `commands/ticket_flow.rs`
  runs it inside the PR sweep and reports each move back with the rows.
- **GitHub** is REST, plus GraphQL for review threads. Refs go through
  `path_segment`. `head` goes in the query, not the path, since `#` and `+`
  in branch names cut a path short. Listing is `state=all` wherever history
  matters, since `state=open` drops a PR the moment it closes.
- **Slack** accepts either a bot token or a webhook. Posts are gated by
  `slack_allows` in the backend and clipped to Slack's 3000-character
  section limit.

## The frontend

- **`Watchers`** (`Watchers.tsx`) holds every poll and event listener and
  draws nothing. It is kept apart from the views because together, a pane
  poll re-rendered whatever view was open.
- **Refreshes go through `coalesce`** (`store.ts`). A poll that finds a
  call in flight shares it. An explicit refresh queues one follow-up
  shared by everyone who asked. Dropping the later call outright once
  dropped the refresh that mattered.
- **A late answer never overwrites a newer one.** Look for `prsFetchedAt`,
  and for sequence refs (`loadSeq`, `searchSeq`, `handoffAsk`) and
  `current` flags in effects. Copy the pattern for anything async whose
  inputs can change while it runs.
- **Double actions are guarded by a ref**, not state. Two clicks in one
  frame both saw the state false (`Terminals.tsx`, `ChatView.tsx`).
- **`TaskMain` is keyed by task id.** Everything under it remounts on a task
  switch, so per-task state cannot leak into the next task.
- **UI kit**: `ui.tsx`. `Modal` (Esc and backdrop close it, `busy` blocks
  that), `Field`, `Combo`, `Confirm` (never `window.confirm`: the webview
  returns true without drawing anything), `ContextMenu`, `Switch`,
  `Spinner`, `BusyOverlay`. One stylesheet, `styles.css`, with theme tokens
  on `:root` (dark only) and plain kebab-case classes.

## Why it is shaped this way

- **A worktree per repo per task**, not branches switched in one clone.
  Agents work in parallel, and a checkout switch under a running agent
  loses its work.
- **Worktrees belong to the app's copy, not the user's clone.** When they
  were the clone's, re-cloning it (2026-09-23) cut seven task folders off
  at once, and a `git worktree prune`, `gc` or branch deletion there could
  do the same. The clone is the user's to do anything with. The cost: task
  branches show in it only after a push and a fetch.
- **One branch name across a task's repos**, and one folder with the repos
  as siblings. A ticket that touches `api` and `web` is one piece of work,
  and the agent can see both.
- **Agents talk to the app, not to Jira or GitHub.** Credentials stay in
  one keychain item. Writes go through one set of rules (confirm, Slack
  switches), and every agent gets the same tools whatever CLI it is.
- **State from reports, not guesses.** Output timing read a two-day-idle
  agent as working, because Claude Code repaints its prompt.
- **Banners from the backend.** The webview's timers are throttled
  precisely when the window is away, which is when banners matter.
- **Whole-file config**, one JSON file, written atomically. Simple enough
  to inspect and to back up. The cost is that two instances cannot share
  it, hence a separate identity for the dev build.
- **Ceilings, not settings** (`MAX_PANES`, `RESTORE_LIMIT`). They stop a
  runaway; nobody needs to tune them.
- **The git CLI, not libgit2**, so the user's hooks, credential helpers and
  signing keep working. The price is the config table under "Git".
- **Handoff moves the work, not the conversation.** No CLI can resume
  another's session, and translating transcripts would break on every
  release. Running out of budget is exactly when an agent cannot summarise
  itself, so the briefing is built from git and the terminal instead.
- **The ticket is the hub.** Sibling PRs are linked through one comment on
  the Jira ticket, not to each other. Cross-linking N PRs needs a second
  pass over every body once they all exist, and it rots when a repo is
  added. The ticket is already where non-engineers look.
- **Chat is the user's own CLI in a terminal**, not a chat UI. No second API
  key, no second set of tool integrations, and the user's own MCP servers
  come along.
- **Connection status comes from the config, not the keychain.** A config
  section is written only after its token verified, so launching the app
  reads the keychain zero times and nothing prompts until an API is called.
- **Interface scale and terminal text size are separate.** xterm measures a
  cell grid, and a page zoom leaves that fractionally out.

## Things that have bitten

Each of these is recorded at the code it shaped. Leave those comments alone.

- **A GUI app's working directory is `/`**, and every child inherits it. A
  CLI started at `/` treats the disk as its project, and macOS asks for
  Photos, Downloads and Music. `run()` in `lib.rs` sets it to `$HOME` once,
  so a new spawn site cannot forget.
- **A GUI app does not see the login shell's PATH**, so `~/.local/bin` and
  friends were missing and agent CLIs "not installed". `shellenv.rs` asks
  the login shell once, behind a marker so a chatty `.zshrc` is not read as
  the environment, with a timeout.
- **macOS walls off other apps' folders**, `~/Library/Containers/…`,
  from an app the user never allowed in, and everything it spawns. An ssh
  config reading a key agent's `.pub` from there (Secretive) worked in a
  terminal and failed on every fetch here. `explain` in `git.rs` says so
  and what to do. A best-effort fetch that fails says nothing, so it went
  unseen until Sync showed a row per repo.
- **A terminal drops a long typed line.** macOS keeps about 1 KB of
  input for a program that is not reading in raw mode, and threw away
  the start of a 1,090-byte conflict hand-off every time. `submit`
  refuses more than `MAX_TYPED`; `commands::hand_over` leaves longer
  text in a file and types where it is (PANE-11).
- **A panic that crosses into AppKit aborts** with no location. The panic
  hook in `lib.rs` writes `~/Library/Logs/villain-layer/panic.log` first,
  and must not panic itself.
- **portable-pty's `ChildKiller::kill` is a SIGHUP to the leader.** An agent
  ignoring SIGTERM ignored that too, and ran on after its pane was gone.
- **xterm reports only its own grid changes**, so a process spawned at
  100×30 stayed there if the first fit happened to agree. Push the size
  after every fit.
- **A hidden tab's terminal must not be fitted**: fitted while its container
  had no height, it kept those rows.
- **`window.confirm` returns true without showing anything** in the webview.
  Use `Confirm`.
- **Focus follows the terminal.** A pasted token went to the agent because a
  terminal took focus from a settings field (`Terminal.tsx`, `mayTakeFocus`).
- **Two instances on one config erase each other's tasks**, and two on one
  keychain item erase each other's tokens. Hence the dev build's own
  identity.
- **Jira names differ per site** ("Done", "Won't Do", "Closed" are all
  done). Pre-selecting the first done transition once closed a merged ticket
  as Won't Do.
- **GitHub allows 30 searches a minute.** Refreshing the review queue on
  every settings save spent them.
- **Lists that page were read as one page**: review threads, reviews,
  createmeta fields, check runs. Each looked complete and was not.

## Known debt

Real, known, and not to be copied. Each is its own change when someone
picks it up.

- `FileIssueDialog` and `CreateEpicDialog` each inline their own copy of the
  required-fields logic. `CreateTaskDialog` uses `tickets/RequiredFields.tsx`,
  which is the one to use.
- `CreateTaskDialog` and `StartWorkDialog` duplicate the base-branch effect.
- Three `ago()` time formatters (`Terminals.tsx`, `AgentsView.tsx`,
  `ReviewsView.tsx`) differ slightly. There are four copies of the
  copy-to-clipboard-with-toast code.
- `DiffView` has its own "which agent gets this" flow instead of
  `useAgentTarget`.
- The terminal theme in `Terminal.tsx` repeats the CSS tokens' values.
- `styles.css` defines `.app`, `.ticket .top` and `.epic-head .title` twice.
- Tests for tasks, Jira, panes and Slack live in `commands/mod.rs` instead
  of beside their code.
- Context files (`AGENTS.md`/`CLAUDE.md` in a task folder) are written with
  `fs::write`, not `agents::replace_file`, so a CLI starting at that moment
  can read a half-written file.
- The untracked-file path of `git::file_diff` reads `dir.join(path)` without
  checking that the path stays inside the worktree.
