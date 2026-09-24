# Contributing

## Setup

```bash
bun install
git config core.hooksPath .githooks   # the pre-commit guard, once per clone
bun run check                         # everything should pass before you start
```

You need Rust (stable), Bun, and the Xcode Command Line Tools.

## The loop

1. **Branch** from `main`.
2. **Write down the requirement.** Find the feature in
   [`docs/features.md`](docs/features.md). If you are changing what it does,
   change its requirements first. A new feature gets a new section from
   the template at the bottom of that file. Reviewers read that diff before
   the code.
3. **Build it** following [`docs/recipes.md`](docs/recipes.md) for the kind
   of change it is.
4. **Verify it** ([`docs/testing.md`](docs/testing.md)): `bun run check`, a
   test for a bug fix, and the mock harness for a UI change.
5. **Open a pull request.** The template asks what the change does, which
   requirements it touches, and what was and was not verified.

Keep pull requests small: one behaviour each. A refactor goes in its own
PR, with no change in behaviour.

## Working with a coding agent

The repository is set up for it. Claude Code, Codex, Copilot, Cursor,
OpenCode and Gemini all read [`AGENTS.md`](AGENTS.md) (`CLAUDE.md` and
`GEMINI.md` point to it).

What works well:

- **Ask for the requirement and a plan first**, before any code: "Read
  AGENTS.md and the Tasks section of docs/features.md, then tell me which
  requirements this touches and which files you would change."
- **Point it at the recipe** for the change ("follow Add a Tauri command in
  docs/recipes.md").
- **Make it run `bun run check`** and read the output itself. Every guard
  failure says what to do.
- **Ask it what it did not verify.** An agent that says "everything works"
  after a UI change it never looked at has not verified it. The mock
  harness exists so it can.

What to watch for in review:

- `// guard: allow …` exceptions. Each needs a reason you agree with.
- A raised file-size ceiling in `scripts/guard.ts`. Was a split possible?
- Deleted or "tidied" comments. Here they are often the only record of a
  bug.
- New dependencies, new settings, new polling.
- A second copy of something that already exists (a helper, a hook, a
  store action).

## Pull request review checklist

- [ ] The requirement in `docs/features.md` matches the code.
- [ ] `bun run check` passes (CI runs it).
- [ ] Bug fixes have a test that fails without them.
- [ ] Nothing that waits runs on the main thread or directly on the async
      runtime.
- [ ] No token, URL-with-secret or personal path in code, logs or errors.
- [ ] No hardcoded Jira names, project keys, GitHub orgs or hosts.
- [ ] UI changes were checked in the mock harness, or the PR says why not.
- [ ] What was not verified is listed.

## Commit messages

The subject is a sentence about what is better for the user, in the
imperative, with a full stop: *Keep new files when a task is finished.* The
body says why: what went wrong before, and why the fix is shaped the way it
is. Name requirement ids where they apply.

## License

A contribution is licensed as the project is, under MIT or Apache-2.0 at
the user's option (see the README), unless you say otherwise when you
submit it.
