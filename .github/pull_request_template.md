## What changes for the user

<!-- One or two sentences. A refactor says "nothing", and why it is worth it. -->

## Requirements

<!-- Ids from docs/features.md this adds, changes or relies on, e.g. TASK-7.
     A behaviour change updates docs/features.md in this PR. -->

## Verified

- [ ] `bun run check`
- [ ] A test that fails without this change:
- [ ] Mock harness (`/mock.html`), for a UI change:
- [ ] The real app / the agent CLI, where only that can show it:

## Not verified

<!-- What you could not run or check, as a list. "Nothing" is an answer. -->

## Review notes

- [ ] No new `// guard: allow` exception, or each has a reason worth agreeing with
- [ ] No raised size ceiling in `scripts/guard.ts`, or the PR says why a split would be worse
- [ ] No new dependency, setting or polling, or the PR says why
