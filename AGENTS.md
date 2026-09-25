# Working on Villain Layer

This file is for anyone changing this repository, person or coding agent.
Claude Code, Codex, Copilot, Cursor, OpenCode and Gemini all read it
(`CLAUDE.md` and `GEMINI.md` are links to it). Read it all before your first
change; it is short on purpose.

Villain Layer is a macOS desktop app (Tauri 2: Rust backend, React 19
frontend) that runs coding-agent CLIs in terminals. Each **task** is one
ticket, given a git worktree of every repository it touches. The app connects
those tasks to Jira, GitHub and Slack. What it does, feature by feature, is in
[`docs/features.md`](docs/features.md). How it works is in
[`docs/architecture.md`](docs/architecture.md).

## The workflow

1. **Understand the requirement first.** Find the feature in
   `docs/features.md` and read its requirements (`TASK-3`, `PANE-7`, …).
   They are the contract. If your change adds or alters behaviour, change the
   requirement **first**, in the same commit as the code. If you cannot say
   what the requirement is, ask; do not guess at it in code.
2. **Find where it goes.** Use the map below. [`docs/recipes.md`](docs/recipes.md)
   has step-by-step instructions for the common changes: a command, a setting,
   an event, a dialog, an agent CLI, an MCP tool, a git operation, an
   integration call. Follow the recipe. Most mess starts as a shortcut past one.
3. **Make the smallest change that meets the requirement.** Prefer extending
   what exists over adding a parallel version of it: one helper, one store
   action, one way to run blocking work.
4. **Prove it.** `bun run check` must pass. It runs the guard, the
   frontend's tests, tsc, the frontend build, clippy and the Rust tests.
   A UI change is looked at and
   clicked through in the mock harness ([`docs/testing.md`](docs/testing.md)).
   A bug fix comes with a test that fails without it.
5. **Say what you did not verify.** An agent CLI you could not run, a Jira
   flow tried only against the mock, an untested macOS behaviour. Put it in
   the commit or PR as a list, not in prose.
6. **Commit in small pieces.** One behaviour per commit. The subject is a
   sentence saying what is better for the user ("Keep new files when a task
   is finished."). The body says why and what broke before.

## Rules

Each rule has cost something before. A rule marked **(guard)** is checked
by `scripts/guard.ts`. Its failure message says what to do, and
[Guard rules](#guard-rules) below lists every one.

### Keep the window responsive

- **Nothing that waits runs on the main thread.** A plain
  `#[tauri::command] fn` runs there. If it shells out, walks the disk, waits
  on a child or the network, no event reaches the webview until it returns:
  terminals stop printing and the window stops answering. Such work goes in
  an `async fn` command, inside `commands::blocking(app, |state| …)`, or
  `commands::off_runtime(|| …)` when no state is needed. **(guard)**
- **Nothing that waits runs on the async runtime either.** Its worker threads
  are shared with the MCP server that agents talk to and with the HTTP clients.
  Git or process work in an `async fn` goes through the same two helpers.
  **(guard)**
- **Push, don't poll.** macOS throttles a hidden webview's timers. Every poll
  is also work repeated whether or not anything changed. State that changes
  in the backend reaches the UI as an event (see the events table in
  `docs/architecture.md`). A new `setInterval` needs a stated reason.
  **(guard)** Anything that must happen while the window is away (banners,
  the dock count) belongs to the backend: `attention.rs`, `news.rs`.
- **Subscribe narrowly.** Components read the store with a selector,
  `useStore((s) => s.tasks)`, never the whole store. A refresh that finds
  nothing changed must not produce a new array (`samePanes`, `sameTasks`).
  Concurrent refreshes share one call through `coalesce` in `store.ts`.
- **Terminals are pooled.** One xterm per pane lives for the pane's
  lifetime and is moved between views, never rebuilt (`Terminal.tsx`). Only
  a terminal on screen is fed. Do not mount xterm anywhere else.

### Don't take the agents down with the app

- **No panics in shipped code.** Release builds use `panic = "abort"`, so a
  panic anywhere quits the app and every agent in it. No `unwrap`, `expect`,
  `panic!`, `todo!` or `unreachable!` outside tests. Return an error.
  **(guard)**
- **An agent is stopped, then killed.** Use `PtyManager::stop` / `close`,
  which send SIGTERM, wait, then SIGKILL the process group. portable-pty's
  own `kill` is only a SIGHUP. In the UI, call `markStopping` before
  `killPane`, or a deliberate stop is reported as a crash (and posted to
  Slack as "finished").
- **Config writes are whole-file and non-reentrant.** Inside
  `config.update(|c| …)`, never call `config.read()` or another accessor:
  the locks are not reentrant and it deadlocks. Every new config field needs
  `#[serde(default)]`, or one missing key makes the whole file unreadable
  and the app starts empty.

### Keep secrets secret

- **Tokens live in the keychain** (`secrets.rs`), never in `config.json`,
  never on a command line (`ps` shows it to every user), never in an error
  message. The one exception is the hook token on curl's command line, which
  can only set a status dot.
- **A saved token goes only to the origin it was saved for** (`stored_or`).
  Jira auth headers go only to the Jira site's own origin, including for
  icons.
- **Anything from outside that reaches a URL path or a git argument is
  validated.** Jira keys go through `segment()`, git refs through
  `path_segment()`. Branch and base names that start with `-` are refused.
  A ticket's text is written by someone else.
- **Agents never get Jira, GitHub or Slack credentials.** They use the app's
  MCP server, and anything that changes shared state needs `confirm: true`.

### Git

- **Only `git.rs` (and `git/`) runs git.** Its `run` is private. Add a
  named function there. That is where the flags that defend against the
  user's own git config live: `--no-ext-diff`, `--untracked-files=normal`,
  `--ff`, `-c` overrides. **(guard, and the compiler)**
- **Worktrees belong to the app's copy of a repo, never the user's clone.**
  New ones are cut from `commands::repo_for(state, project)`. For an
  existing one, ask it which repository owns it: `commands::owner_of`.
  `Project.path` is where the user's clone is, for showing and copying,
  not for running git. Only Sync and Clean up write to it, and only as
  `REPO-7` and `REPO-8` allow: a fast-forward of its default branch, or
  deleting a task branch whose every commit the app's copy has.
- **Never `--force`.** A rebased branch is pushed with
  `--force-with-lease=<branch>:<sha>`, from `Checkout.push_lease`, and only
  through `git::push`.
- **A branch is measured from its recorded branch point**
  (`Checkout.base_commit`), not from its base branch, which moves.
  `git::baseline` decides.

### Integrations

- **Nothing hardcodes one Jira or one GitHub.** Status *categories* (`new`,
  `indeterminate`, `done`) and issue-type *hierarchy levels* mean the same on
  every site; names do not. Required fields come from `createmeta`. Ask the
  site what it wants.
- **Page everything that pages.** Several bugs were "only the first 30 / 50 /
  100 were read, and the UI said all clear". Keep the cap explicit, and tell
  the user when it is reached (`more: true`).
- **A partial failure stays visible.** A multi-repo operation returns a row
  per repo (`RepoResult`, a row's `error`), not one error for everything and
  not silence.

### Keep the code in one piece

- **Dead code goes.** A removed command is removed from its module, from
  `generate_handler!` in `lib.rs`, from `api.ts`, and from the types on both
  sides, in the same change. **(guard)**
- **The frontend reaches the backend only through `src/lib/api.ts`.**
  Argument names are camelCase, since Tauri renames the Rust parameters.
  **(guard)** Types returned are mirrored by hand in `src/lib/types.ts`, and
  change with the Rust struct.
- **Files stop growing at their ceiling** and get split along a seam
  instead. **(guard)**
- **Comments say why, not what.** The valuable comment records the failure
  that made the code look like this. Many here are the only record of a bug.
  Never delete or "summarise" one without deleting the code it explains.
- **A doc comment belongs to the thing under it.** Inserting a function
  between a `///` block or `#[tauri::command]` and its function silently
  re-parents it. This has happened twice.
- **Tests read as sentences** (`a_rebased_branch_pushes_only_over_what_it_was_rebased_against`),
  sit at the bottom of the file they test (**guard**), and use real git in a
  temp directory rather than mocks of it.
- **No new dependency without saying why** in the PR: what it replaces, and
  why the standard library or an existing dependency does not do.

## Guard rules

`bun run guard` (included in `bun run check`). A rule's exception is written
beside the code, with a reason: `// guard: allow <rule> — <why>`.

| Rule | What it catches | Fix |
|---|---|---|
| `main-thread` | git, a process, sleep or a directory walk in a plain `#[tauri::command] fn` | Make it `async`, put the work in `commands::blocking` |
| `runtime-blocking` | the same, directly in an `async fn` command | `commands::blocking` or `commands::off_runtime` |
| `command-registered` | a `#[tauri::command]` missing from `generate_handler!` | Register it, or delete it |
| `dead-command` | a registered command nothing in `api.ts` invokes | Delete it everywhere |
| `api-command-exists` | `api.ts` invoking a command that does not exist | Fix the name |
| `api-args` | `api.ts` sending an argument name the command does not take, or leaving out a required one | camelCase the Rust parameter's name |
| `dead-wrapper` | an `api.*` wrapper nobody calls | Delete it |
| `api-only` | `invoke` imported outside `api.ts` | Add a wrapper to `api.ts` |
| `event-sync` | an event emitted that nothing listens for, or listened for that nothing emits | Fix the name, or remove the dead end |
| `poll` | a new `setInterval` without a reason | Use an event; or poll only while `appActive`, and say why |
| `panic` | `unwrap`, `expect`, `panic!`, `todo!`, `unreachable!` or `dbg!` outside tests | Return an error |
| `git-only-in-git-rs` | `Command::new("git")` outside `git.rs` | A named function in `git.rs` |
| `tests-last` | a Rust test module that is not at the end of its file | Move it |
| `console` | `console.log`, `console.debug`, `console.info` or `debugger` in `src/` | Remove it; use `toast` for the user |
| `file-size` | a file past its line ceiling | Split it (`docs/recipes.md`); or raise the ceiling in `scripts/guard.ts` and say why |
| `docs-sync` | an MCP tool, agent CLI or event missing from the docs | Add it to `docs/features.md` / `docs/architecture.md` |

## Commands

```bash
bun install
bun run check          # everything below, in order; must pass before a commit
bun run guard          # scripts/guard.ts alone, about a second
bun run build          # tsc && vite build
bun run dev:app        # the app, as a dev build under its own identity
bun run dev            # Vite alone; the mock harness is at /mock.html
bun run release:mac    # release build, installed over /Applications
```

Releases for everyone else come from a version tag, on GitHub:
[`docs/releasing.md`](docs/releasing.md).

```bash
cd src-tauri
cargo clippy --all-targets -- -D warnings
cargo test --lib
```

`cargo check` does not compile tests. It passes while a test still calls a
signature you changed.

**One dev server at a time.** Vite holds port 1420 with `strictPort`, and a
second `tauri dev` fights the first over the same files. If one is running,
use it: the mock harness is served by it too.

**Two copies of the app never share state.** The dev build has its own
identifier (`eu.codevillain.villain-layer.dev`), config folder and keychain
item. Each copy reads its config once at launch and writes it back whole, so
two copies on one file erase each other's tasks. `release:mac` refuses to run while the
installed app is open. Replacing the bundle under a running app kills it
and every agent in it. `cargo` relinking `target/debug` under a running dev
build does the same.

## Where things are

| Path | What |
|---|---|
| `src-tauri/src/lib.rs` | startup, `generate_handler!` (every command's registration) |
| `src-tauri/src/commands/` | every Tauri command, by area: `projects`, `repos` (health, Sync, Locate), `cleanup`, `tasks`, `panes`, `diff`, `jira`, `github`, `slack`, `settings`; `mod.rs` has `AppState`, `blocking`, `off_runtime` |
| `src-tauri/src/config.rs` | `AppConfig` (what `config.json` holds), `ConfigStore`, migration |
| `src-tauri/src/pty.rs` | panes: spawning, output buffer and throttling, stop/kill, activity state |
| `src-tauri/src/agents.rs` | the agent CLI catalogue and how each reports its state |
| `src-tauri/src/mcp.rs` | the app's MCP server for agents, and `/hook` for their status reports |
| `src-tauri/src/git.rs`, `git/` | the only place that runs git; `store.rs` is the app's own copy of each repo, `upkeep.rs` its Sync and Clean up |
| `src-tauri/src/attention.rs` | agents that need you: dock count, banners |
| `src-tauri/src/news.rs` | new review requests and tickets, for banners |
| `src-tauri/src/messages.rs` | the message center's log, kept in `messages.json` |
| `src-tauri/src/integrations/` | Jira, GitHub and Slack HTTP clients |
| `src-tauri/src/secrets.rs` | tokens in the keychain |
| `src-tauri/src/shellenv.rs` | the login shell's environment, for spawned processes |
| `src/App.tsx` | the shell and the top bar |
| `src/Watchers.tsx` | `Watchers`: every poll and event listener, and the toasts they raise |
| `src/store.ts` | all frontend state and its refresh actions |
| `src/lib/derive.ts` | pure helpers over that state (`paneState`, `needsYou`, …) |
| `src/lib/target.ts`, `goto.ts` | where a click on a banner, toast or message lands (`target.rs` writes them) |
| `src/lib/api.ts` / `src/lib/types.ts` | the IPC surface, and the Rust types mirrored (GitHub's in `types-github.ts`, re-exported) |
| `src/components/ui.tsx` | `Modal`, `Field`, `Combo`, `Confirm`, `ContextMenu`, `Switch`, … |
| `src/components/` | one file per view or dialog; `sidebar/`, `tickets/` for those areas |
| `src/__mock__/`, `mock.html` | the UI against a fake backend, for looking and clicking |
| `scripts/guard.ts` | the guard |

## Limits

`MAX_PANES` (32, `pty.rs`) and `RESTORE_LIMIT` (12, `commands/panes.rs`)
are ceilings that stop a runaway, not preferences. Do not make them
settings.

## Done means

- [ ] The requirement in `docs/features.md` says what the code now does.
- [ ] `bun run check` passes.
- [ ] A bug fix has a test that fails without it.
- [ ] A UI change was clicked through in the mock harness, or the PR says it was not.
- [ ] No guard exception without a reason a reviewer can judge.
- [ ] What was not verified is listed.
