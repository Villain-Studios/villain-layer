# Working on Villain Layer

Tauri 2 (Rust) + React 19. [`README.md`](README.md) explains what the app is
and why it is shaped the way it is; this file is for the part that only
matters once you are editing it.

## Commands

```bash
bun run dev:app      # the dev build, under its own identity — see below
bun run build        # tsc && vite build
bun run release:mac  # release build, installed over /Applications
```

```bash
cd src-tauri
cargo clippy --all-targets   # kept clean; a warning is a failure
cargo test --lib
```

**`cargo check` does not compile tests.** It will pass while a test still
calls a signature you changed. Run `cargo test` before believing a Rust
change is done.

## Two instances

Using the app while changing it means two copies running, and they cannot
share state: the config is read once at launch and written back whole, so two
instances pointed at one file take turns erasing each other's tasks.

`bun run dev:app` merges `src-tauri/tauri.dev.conf.json` over the normal
config, which gives the dev build the identifier `dev.villain.layer.dev` and
its own config directory. Only one at a time — Vite holds port 1420 with
`strictPort`.

`release:mac` refuses while the app is running. Replacing a bundle under a
live process kills it and takes its agents with it, and the same is true of
`cargo` relinking `target/debug/villain-layer` under a running dev build.

## Conventions

- **Comments say why, not what.** The interesting comment is the one that
  records the failure that made the code look like this. Several in here are
  the only surviving account of a bug; do not summarise them away.
- **Nothing hardcodes this Jira or this GitHub.** Status *categories*
  (`new` / `indeterminate` / `done`) and issue-type *hierarchy levels* mean
  the same on every site; names do not. Ask the site what it wants.
- **Dead code goes.** A command removed here is removed in `lib.rs`, in
  `api.ts`, and in the types on both sides, in the same change.
- **A doc comment belongs to the thing under it.** Inserting a function
  directly above another one's `#[tauri::command]` or `///` block silently
  re-parents that comment. This has happened twice.

## Where things are

| | |
|---|---|
| `src-tauri/src/commands/` | every Tauri command; the bulk of the backend |
| `src-tauri/src/pty.rs` | panes, the PTY, output throttling, limit detection |
| `src-tauri/src/git.rs` | the only place that shells out to `git` |
| `src-tauri/src/agents.rs` | the agent CLI catalogue — add one here |
| `src-tauri/src/mcp.rs` | the app's own MCP server, and `/hook` for agents' reports |
| `src-tauri/src/attention.rs` | agents that need you: the dock count and their banners |
| `src-tauri/src/news.rs` | new review requests and tickets, for banners |
| `src-tauri/src/integrations/` | Jira, GitHub, Slack clients |
| `src/store.ts` | all frontend state; also the derived helpers |
| `src/components/ui.tsx` | `Modal`, `Field`, `Combo`, `Confirm`, `ContextMenu` |

## Things that have bitten

- **A plain `#[tauri::command] fn` runs on the main thread.** Anything in it
  that shells out to git, walks the disk or waits on a child holds the event
  loop: no other invoke is delivered and no event reaches the webview, so
  terminals stop printing and the window stops answering the mouse. A command
  that does any of that takes `AppHandle` and goes through
  `commands::blocking`, which is `spawn_blocking` — not the async runtime,
  whose workers are shared with the MCP server and the HTTP clients. `async
  fn` commands are already off it.
- **A GUI app's working directory is `/`.** Every child inherits it, and a
  coding CLI started at the filesystem root treats the whole disk as its
  project — which on macOS means a permission prompt for Photos, Downloads
  and Music. `run()` in `lib.rs` sets it once, which is the only place a new
  spawn site cannot forget.
- **A terminal outlives the view that shows it.** `Terminal.tsx` keeps one
  xterm per pane in a pool and moves its element into whichever view mounts
  a `TerminalPane`; unmounting only parks it, and it is disposed when the
  pane leaves the store's list. Only a terminal on screen is fed — a hidden
  one detaches, and `pty_attach` sends it what came after the last byte it
  drew. One xterm per mount meant every task or view switch rebuilt it and
  replayed 256KB of scrollback.
- **A terminal has to be told its size.** xterm emits `onResize` only when
  its own grid changes, so a process spawned at the default 100×30 stays
  there if the fit happens to agree. Push the dimensions after every fit.
- **`AppConfig` does not deny unknown fields.** Removing a field silently
  drops that key from existing configs on the next write. That is usually
  what you want; it would not be if the field held anything.
- **A branch is measured from its recorded branch point**, not from its base
  branch, which moves. Where both exist, `git::baseline` decides — and a
  worktree without a recorded point falls back to the merge base, which
  credits the branch with everything merged in from elsewhere. Merging the
  base in moves the point to what was merged; a point the branch never
  reached (a merge abandoned half way) is passed over.
- **The user's git config applies to every git the app runs.** `merge.ff =
  only` made every update from base fail with "Not possible to fast-forward"
  until the merge said `--ff` itself. `status.showUntrackedFiles = no` made a
  worktree holding only new files read as clean — to our status and to git's
  own check in `worktree remove` — and it was removed with them. A global
  `diff.external` answered the Diff view with no hunks. Anything the app
  relies on git doing, it spells out — `-c` for what has no flag, since an
  older git ignores a config key it does not know but refuses an option it
  does not.
- **An agent's output does not say whether it is working.** Claude Code
  repaints its prompt every few seconds while it sits idle, so "printed
  recently" read a two-day-idle agent as working. Each CLI that can say for
  itself has an `Integration` in `agents.rs` — hooks, a plugin, or its window
  title — set up per launch by `agents::prepare_launch`, and posts land on
  `/hook/<pane>` on the app's server, with a token only `/hook` accepts: the
  shell puts it on curl's command line, where `ps` shows it.
  `pty::PaneMeta::state` believes those over output; output only decides for
  CLIs that report nothing, and not in the moment after the app sent the
  pane something.
- **portable-pty's `ChildKiller::kill` is a SIGHUP to the leader.** Not
  SIGKILL: an agent ignoring SIGTERM ignored it too, and ran on after its
  pane was taken off the list. `PtyManager::force_kill` is the one that
  insists.
- **A rebased branch is pushed with a lease, never `--force`.** The commit the
  remote branch was at is recorded on the checkout (`push_lease`) when it
  rebases and cleared by the next push. Any new push site goes through
  `git::push` with it.
- **GitHub's PR listing is state-scoped.** `state=open` drops a PR the moment
  it closes, so anything that wants history has to ask for `state=all`.

## Limits

`MAX_PANES` (32, `pty.rs`) and `RESTORE_LIMIT` (12, `commands/panes.rs`) are
ceilings that stop a runaway, not preferences. Do not turn them into
settings.
