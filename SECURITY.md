# Security

Villain Layer holds your Jira, GitHub and Slack tokens. It runs an MCP
server and a status endpoint on localhost for its agents. And it turns text
other people wrote, tickets and review comments, into prompts, branch names
and git arguments. A flaw in any of these matters, so please report one
privately.

## Reporting a vulnerability

- **Report a vulnerability** on the repository's
  [Security tab](https://github.com/Villain-Studios/villain-layer/security/advisories/new),
  or
- email **codevillain@proton.me**.

Please do not open a public issue for it. Say which version you ran
(Settings → General → Version), what an attacker needs (a ticket they
wrote, a repository they control, a process on the same Mac), and how to
reproduce it.

You will hear back within a week. The fix ships as a new release, and the
advisory is published with it, crediting you unless you would rather not.

## Supported versions

Only the latest release gets fixes.

## What counts

- A token leaving the keychain: into a file, a log, an error, a command
  line, or to any origin other than the one it was saved for.
- Anything other than the app's own agents driving its MCP server or
  status endpoint, or an agent changing shared state (a ticket, a pull
  request, a Slack message) without the confirmation the tool asks for.
- Text from a ticket, a pull request or a repository that makes the app
  run a command, or reaches git as an option.
- The app writing outside the places listed in
  [`docs/features.md`](docs/features.md#14-what-the-app-writes-on-your-machine).

What an agent CLI does with the permissions you give it in its own
terminal is that CLI's matter, not the app's.
