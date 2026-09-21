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
- **A terminal has to be told its size.** xterm emits `onResize` only when
  its own grid changes, so a process spawned at the default 100×30 stays
  there if the fit happens to agree. Push the dimensions after every fit.
- **`AppConfig` does not deny unknown fields.** Removing a field silently
  drops that key from existing configs on the next write. That is usually
  what you want; it would not be if the field held anything.
- **A branch is measured from its recorded branch point**, not from its base
  branch, which moves. Where both exist, `git::baseline` decides — and a
  worktree without a recorded point falls back to the merge base, which
  credits the branch with everything merged in from elsewhere.
- **GitHub's PR listing is state-scoped.** `state=open` drops a PR the moment
  it closes, so anything that wants history has to ask for `state=all`.

## Limits

`MAX_PANES` (32, `pty.rs`) and `RESTORE_LIMIT` (12, `commands/panes.rs`) are
ceilings that stop a runaway, not preferences. Do not turn them into
settings.
