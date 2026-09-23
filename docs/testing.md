# Verifying a change

"It compiles" is not verified. Neither is "the test passes" for a change
the tests do not reach. This page is how to show a change works, at each
level, and what to say when you could not.

## `bun run check`

Run before every commit. It stops at the first failure.

| Step | What it proves |
|---|---|
| `bun scripts/guard.ts` | the rules in `AGENTS.md` that tools cannot check: command and event sync, nothing blocking the main thread or runtime, no panics, file sizes, docs listing every tool, CLI and event |
| `tsc` | the frontend type-checks, including the mock harness against `types.ts` |
| `vite build` | the frontend bundles; `tauri::generate_context!` needs `dist/` for the Rust steps |
| `cargo clippy --all-targets -- -D warnings` | the backend, tests included, compiles clean. A warning is a failure |
| `cargo test --lib` | the Rust tests |

`cargo check` alone does not compile the tests, so it passes while a test
still calls a signature you changed.

The pre-commit hook (`.githooks/pre-commit`, enabled with
`git config core.hooksPath .githooks`) runs the guard and `tsc`, which take
seconds. CI runs all of `bun run check`.

## Rust tests

They sit at the bottom of the file they test, in `mod tests`, named as the
sentence that should hold:
`a_branch_deleted_on_the_remote_is_forgotten_here`.

- **Git**: real git in a temp dir, never a mock of it. In `git.rs`,
  `fixture()` is a one-commit repo, and `remote_and_worktree()` is a bare
  remote, a seed clone, a local clone and a worktree. Other modules' tests
  make repos with `git::run_for_tests`. The tests run under *your* global
  git config, so a test must pass whatever it says (the app's own flags
  are what make that true).
- **Panes**: `tauri::test::mock_app()` gives an `AppHandle` without a
  window. The PTY tests spawn real processes (`/bin/sh`, `/bin/sleep`).
- **Pure logic** (state machines, parsers, prompts, JQL): plain inputs and
  outputs. Parsers are tested against JSON shaped like the real API's, with
  made-up names.
- **Timing**: `echo_latency` is `#[ignore]`, a measurement rather than a
  test. Run it with `cargo test --lib echo_latency -- --ignored --nocapture`.

A bug fix adds the test that fails without the fix. A test that cannot fail
proves nothing: check it fails by breaking the code once.

## The UI: the mock harness

The native window can be screenshotted but not clicked by an agent. The
mock harness runs the real frontend in a browser against a fake backend,
so a UI change can be looked at and driven.

```bash
bun run dev              # or use the Vite that `bun run dev:app` already started
open http://localhost:1420/mock.html
```

Only one Vite runs at a time, on port 1420. If `dev:app` is running, the
harness is already served. Don't start a second server.

**Setting the scene** with URL parameters:

| Parameter | Values | Default |
|---|---|---|
| `scenario` | `busy` (everything connected, agents in every state), `empty` (first launch), `unlinked` (a task whose worktrees lost their repository) | `busy` |
| `view` | `work`, `tickets`, `reviews`, `chat`, `repos` | `work` |
| `task` | a task id from `src/__mock__/world.ts` (`t-login`, `t-audit`) | none, which is the All agents overview |
| `tab` | `terminals`, `diff`, `pr` | `terminals` |

For example: `/mock.html?task=t-login&tab=pr` is the PR panel with a failing
check and a change request.

**Driving it** from the browser console, or a browser tool's JavaScript:

```js
__mock.calls                                  // every command the UI invoked, with arguments
__mock.calls.filter((c) => c.cmd === "spawn_agent")
__mock.on("github_open_prs", () => [{ checkout_id: "c-login-api", repo: "api", ok: false, detail: "rejected" }])
__mock.world.panes[0].activity = "done"; __mock.emit("pty:activity", "pane-claude")
```

- **The data** is in `src/__mock__/world.ts`, typed against `src/lib/types.ts`.
  When a type changes, `tsc` fails until the fixtures follow. Add a
  scenario there for a state you need to show more than once.
- **Answers** are in `answer` in `src/__mock__/boot.ts`. A command it does
  not answer logs `[mock] unanswered <cmd>` in the console and returns
  `null`. If your change calls a new command, add its answer.
- **What it cannot show**: real terminals (panes print canned text), real
  timing, anything the backend decides (state transitions, git, the
  integrations). It tests the UI's handling of answers, not the answers.
- Opening `/` instead of `/mock.html` loads the app without the mock, and
  the console fills with `reading 'metadata'` errors. That is expected.

What to check for a UI change:

- the flow you changed, click by click, with `__mock.calls` showing the
  right command and arguments;
- the empty scenario, and a failure (override the command to throw:
  `__mock.on("x", () => { throw "boom"; })`);
- no new errors in the console;
- a narrow window (the app's minimum is 900×600).

## The real app

Some things only the app can show: terminals, agents reporting their
state, notifications, the keychain, git on real repos.

```bash
bun run dev:app        # dev build: its own config folder and keychain item
```

The dev build is `Villain Layer Dev` (`dev.villain.layer.dev`). It starts
empty, with nothing connected, and has its own config and keychain item, so
it can run next to the installed app. One thing both default to is the task
folder location, `~/.villain-worktrees`: set the dev build's to something
else (Settings → General, for example `~/.villain-worktrees-dev`), so their
task folders and chat rooms stay apart. Quit it before `cargo` rebuilds under
it, or relinking kills it and its agents.

```bash
bun run release:mac    # refuses while the installed app is running
```

- Rust `println!` / `eprintln!` go to the terminal that ran `dev:app`. The
  webview's console is under right-click → Inspect in a dev build.
- A crash writes `~/Library/Logs/villain-layer/panic.log`.
- The config is `~/Library/Application Support/<identifier>/config.json`.
  Quit the app before editing it by hand: the app writes back the whole
  file.

## Agent CLIs

A change to how a CLI is launched or reports its state is only verified by
running that CLI. Start it in a task and watch the dot move: working while
it runs, asking at a permission prompt, done when it stops. Call one MCP
tool from it (`list_tasks`). If you cannot run the CLI (not installed, no
account), say so in the PR. Checking its config schema alone is worth
saying too.

## What to write in the PR

```markdown
Verified:
- bun run check
- New test: `a_…` (fails without the fix)
- Mock harness: Finish task with two done transitions defaults to "Leave as is"

Not verified:
- Gemini: not run end to end (account setup); checked against its settings schema
```
