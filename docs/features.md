# Features and requirements

This is the contract for what Villain Layer does. Each area says what the
user sees, then the requirements (**MUST** statements, each with a stable
id), where the code is, and the gaps we know about.

How to use it:

- **Before changing behaviour**, read the area's requirements. A change that
  breaks one is a bug, unless the requirement changes too, in the same
  commit and deliberately.
- **A new feature adds requirements here first.** Use the template at the
  end. If you cannot write the requirement, the feature is not understood
  yet.
- **Refer to requirements by id** in commits, PRs and test names' comments
  (`TASK-4`). Ids are never reused. A dropped requirement is struck through
  with a note.
- **Known gaps** are real and unfixed. Don't copy the behaviour they
  describe, and don't "fix" one in passing. Each deserves its own change.

The words: a **repository** (repo) is a git clone the user registered. A
**task** is one piece of work, usually one Jira ticket, spanning one or more
repos. A **checkout** is one repo's git worktree inside a task. A **pane**
is a terminal running an agent CLI or a shell, or an agent's conversation
over ACP (§20).

---

## 1. Repositories

The Repos view lists registered repos in collapsible groups. Repos are added
by picking folders, or by scanning a folder (three levels deep) and ticking
what was found. A repo has at most one group; groups are renamed in place.
Each repo says how it is doing, and the view keeps them in shape: **Sync**
brings them up to date, **Locate** follows a clone that moved, and **Clean
up** removes what tasks left behind. A repo can have a check command, which
a loop runs (LOOP-1).

- **REPO-1** A repo MUST be a git work tree root. It is registered by path,
  and its default branch is detected (origin/HEAD, then `main`, then
  `master`, then the current branch).
- **REPO-2** Scanning MUST skip symlinks and dot-folders, and stop at 500
  results.
- **REPO-3** Removing a repo MUST NOT touch the clone or any worktree on
  disk. It drops the repo's checkouts from tasks. It drops tasks left with
  no repo, and stops their panes.
- **REPO-4** Every repo MUST get the app's own copy
  (`<task folder location>/.repos/<name>.git`), made in the background
  when it is added, or when a task first needs it. Task worktrees are cut
  from the copy, so nothing done to the user's clone reaches them:
  re-cloning, moving, deleting, pruning, gc, branch deletion. The copy is
  hard-linked, so it costs little disk and survives the clone being
  deleted. It fetches from the clone's `origin` and carries its repo-local
  git settings. A copy made while the clone had no `origin` fetches from
  the clone, and MUST move to the clone's `origin` once it has one (at
  launch, at Sync, when a task needs it), or task branches are pushed into
  the user's clone. Such a repo is not read as a GitHub repository.
- **REPO-5** Each repo MUST show whether its clone is still where it was
  registered and still the same repository, where it fetches from, whether
  the app's copy exists and when a Sync last reached origin (not when a
  fetch last tried: git rewrites `FETCH_HEAD` before it knows), and how the clone's
  default branch stands against origin's (behind, or with commits of its
  own), and how Update from base updates branches there (UPD-7), which can
  be set. A repo with a problem says what the problem is, with its fix beside
  it, and its group opens. Read when the view opens and after anything here
  changes it, never on a timer.
- **REPO-6** Locate MUST point a repo at another folder without taking it
  out of any task, and only when that folder is the same repository: one
  fetching from where the app's copy does. When the clone is gone, a folder
  of the same name near where it was is offered. Before this, the only way
  was remove and add again, which dropped the repo from every task (REPO-3).
- **REPO-7** Sync MUST fetch origin into the app's copy, forgetting branches
  origin deleted and taking tags origin moved (an Actions repo moves `v1`
  each release; refused, it failed every Sync of a user whose git fetches
  all tags), and carry the clone's repo-local settings again. A failed
  fetch says git's reason, not only that it failed. In the
  clone it only ever fast-forwards the default branch, and only when that
  branch has no commits of its own and, if checked out, no uncommitted
  changes to tracked files. It never merges or rebases there, so how a team
  updates its branches does not matter to it. It reports a row per repo:
  what moved, what was left alone and why. A Sync of several ends with one
  line naming the repos that failed (five, then how many more).
- **REPO-8** Clean up MUST list what it would remove, and why, before it
  removes anything. It pre-selects only what loses nothing, and checks each
  item again as it removes it. It finds:
  - task folders no task uses: pre-selected when they hold only what the
    app generated, empty folders and worktrees with no changes; offered,
    naming the first few, when they also hold files the app did not make
    (an agent CLI's log, a build cache written after the worktree went).
    Never one holding a git repository or worktree at any depth that no
    copy lists, or more than 5000 things the app did not make. A folder
    that gained files since the list was made is not removed;
  - worktrees inside a task folder that are none of its checkouts, on the
    same terms;
  - task branches in the user's clone, left from before REPO-4 or taken
    over by a task (TASK-3), only when every commit on them is in the
    app's copy and they are not checked out;
  - branches in the app's copies that no task uses, pre-selected when every
    commit on them is on origin, or, in repos on the connected GitHub, when
    a pull request has every commit on them: your newest from the branch,
    merged, or, for a branch the app made to review one
    (`review/<repo>-<n>`), that pull request. A squash merge leaves a
    branch's commits on no branch once origin deletes it, so git alone
    took every finished task's branch for unpushed work;
  - your branches on origin that are done with, in repos on the connected
    GitHub, as origin stood at the last Sync. Yours means your newest pull
    request from it is merged or closed, or, with no pull request of yours,
    you wrote its last commit (the app's copy's `user.email`). Done with
    means that pull request merged and nothing was pushed to it since, or
    every commit on it is in the default branch: both pre-selected, since
    GitHub can restore the first from the pull request and the second loses
    nothing. A pull request closed unmerged, or a branch pushed to after
    its pull request merged, is offered, not pre-selected. Never the
    default branch, a branch a task of any build is on, or one an open pull
    request of anyone's comes from. It is deleted only if origin's branch
    is still at the commit it was judged at. Your 300 most recently created
    pull requests in each repo are read; when there are more, it says so.
    A repo GitHub could not be asked about says why;
  - copies no repo uses, never one a worktree still belongs to (the other
    build's, say);
  - git's records, in the app's copies, of worktrees whose folders are
    gone, and staging left by an interrupted move (TASK-12).

  A worktree is removed through git, which refuses one with changes. A
  folder is removed only once it is empty.

Code: `commands/projects.rs`, `commands/repos.rs`, `commands/cleanup.rs`,
`commands/cleanup_folders.rs`, `commands/cleanup_remote.rs`, `git/store.rs`,
`git/upkeep.rs`, `integrations/github/branches.rs`, `ReposView.tsx`,
`AddRepos.tsx`, `CleanUp.tsx`.

Known gaps:
- Removing a repo leaves panes running in *surviving* tasks' checkouts of
  it, and does not rewrite those tasks' context files. Removing a checkout
  does both.
- Sync carries the clone's repo-local settings again, but one removed from
  the clone stays in the copy.
- Removing a repo leaves its copy in `.repos/`. Adding the repo again
  reuses it; Clean up offers it once no worktree belongs to it.
- A branch squash-merged and then deleted on origin has commits that are on
  no remote branch. In a repo not on the connected GitHub, or with no pull
  request of yours from it, Clean up cannot tell it from unpushed work and
  does not pre-select it.
- Task branches are not in the user's clone until pushed and fetched.

## 2. Tasks

A task has a name, a branch (the same in every repo), a folder, and one
checkout per repo. The sidebar lists tasks as working, in review (has an
open PR) or done (every PR merged). Each row shows state dots, the ticket,
the repo count, running agents, uncommitted changes and the review verdict.

- **TASK-1** A task MUST have a folder, `<worktree root>/<branch, / → ->`.
  Each checkout is a subfolder named after its repo. Agents start in the
  task folder, where every repo is a sibling, unless started for one repo.
  A folder that exists or belongs to another task gets `-2`, `-3`, ….
- **TASK-2** The branch MUST be: the explicit name if given; else the
  ticket key, plus `-<suffix>` if given; else `villain/<slug of the name>`.
- **TASK-3** A checkout MUST be cut, in the app's copy of the repo
  (REPO-4), from the freshly fetched `origin/<base>`, not the local base branch, which may be stale. The commit
  it was cut from is recorded as the branch point
  (`Checkout.base_commit`). Everything that measures the branch ("changed",
  the Diff, PR descriptions, handoffs) measures from that point. A branch
  that already exists goes on from its own commits: the copy's, or, when
  only the user's clone has it (started there by hand), the clone's,
  brought into the copy first, or, when only GitHub has it (a pull request
  pushed from elsewhere), GitHub's, fetched and tracked. It is never cut
  again from the base under a name GitHub already has.
- **TASK-4** Creating a task MUST be all or nothing. If any repo fails, the
  worktrees and branches this attempt made are removed, and branches it did
  not make are kept. The task record goes too.
- **TASK-5** A second task for a ticket that already has one MUST need a
  branch suffix.
- **TASK-6** Adding a repo to a task MUST tell the task's running agents,
  typed into each, and rewrite the folder's context files. The new
  checkout is cut from the base the rest of the task shares, when this
  repo has that branch, and otherwise from its own default branch. Removing one
  stops panes in that checkout, and refuses when it has uncommitted changes
  unless forced.
- **TASK-7** Deleting a task MUST check for uncommitted changes *before*
  stopping any agent. It refuses without force: a refusal must not cost the
  conversations. If any worktree cannot be removed, the task is kept with
  just those checkouts, never left as a folder the app cannot see.
- **TASK-8** Finish task (offered once every PR has merged) MUST go in
  order: stop agents, remove worktrees, delete each local branch **only if**
  the merged PR's head or `origin/<base>` contains it, then move the ticket
  to the chosen done-category status. Each step only runs if the one before
  succeeded. Remote branches are never touched.
- **TASK-9** The finish dialog MUST offer only transitions into the `done`
  category. It pre-selects the one into the project's after-merge status
  (TKT-8), or else one only when there is exactly one; otherwise it defaults
  to leaving the ticket as it is. (Done and Cancelled are both done: with no
  status chosen, a finished ticket was left In Progress.)
- **TASK-10** Status (ahead, behind, changes, conflicts) comes from
  `git status` per checkout. Only the selected task and tasks with a running
  pane are asked every poll. The rest reuse a cached status for up to 90
  seconds, so a poll is not one git process per worktree ever made.

- **TASK-11** A checkout whose folder exists but git cannot read MUST say
  so, with the reason, in the task header, the sidebar and the Diff tab.
  It is never shown as clean or empty. It happens when the repository it
  was registered in is deleted or cloned again, which takes
  `.git/worktrees/` and the local branches with it; the files stay in the
  task folder.
- **TASK-12** At launch, before any pane comes back, every worktree still
  registered in a user's clone MUST be moved onto the app's copy (REPO-4)
  in place. The files are untouched, unpushed commits come across, and
  staging survives. A worktree mid-merge or mid-rebase is left for the
  next launch. A folder already cut off (TASK-11) is linked back at the
  commit the status poll last saw it on (`Checkout.last_head`), when the
  copy has that commit. A folder that has become a clone of its own (its
  `.git` a folder, as when it is cloned again in place) is linked back
  too, when that loses nothing: it fetches from the same origin, is on the
  task's branch, has nothing staged, no stash and no merge or rebase in
  progress, every commit on its other branches is already in the copy, and
  the copy's branch has no commit it lacks. Its commits come across, its
  files are untouched (an edit stays an unstaged edit), and its own `.git`
  folder goes. What could not be moved is reported, with the reason.
- **TASK-13** The Start work dialog MUST fill in "Branch
  from" with the default branch of the picked repos when they share one,
  and leave it blank (each repo's own default) when they do not. The box
  follows every change of repos while it still shows the dialog's own last
  suggestion; a base typed by hand stays.
- **TASK-14** Open in Cursor (the task header, the Terminals tab, the
  sidebar's menu) MUST open the task folder in the Cursor IDE, and be
  offered only when the IDE is installed: `Cursor.app` in an Applications
  folder, or a `cursor` command that links into a `.app`. The Cursor agent
  CLI's own `cursor` shim is not the IDE. When opening fails, the user is
  told why.
- **TASK-15** In the app, a task MUST start from work that already
  exists: Start work on a Jira ticket (TKT-1), or Start task on one of
  your pull requests (REV-7). The sidebar offers no task of its own; its
  "+" made one with a name and no ticket, or filed a new ticket and opened
  worktrees for it in one step. A new ticket is filed from Tickets, then
  started. A chat's agent can still make a task with no ticket
  (`create_task`, CHAT-3).

Code: `commands/tasks.rs`, `sidebar/`, `FinishTask.tsx`.

Known gaps:
- A folder cut off before its last commit was recorded, or whose last
  commit was never pushed or copied, cannot be linked back automatically
  (TASK-12). By hand: create the branch at the commit the files match,
  `git worktree add --no-checkout` it somewhere temporary, point the new
  registration and the folder's `.git` file at each other, then
  `git reset` in the folder. Its files are never touched.
- Finishing moves the ticket even when a branch was kept because it was
  not contained. The dialog says so.
- `worktree remove` deletes gitignored files (`.env`, build output) with
  the worktree. Git does this; the confirm says "uncommitted", not
  "ignored".

## 3. Agents and terminals

A task's Terminals tab holds its panes. "+" starts an agent (with an
opening prompt made from the ticket, which can be edited), resumes one, or
opens a shell. When a task has several repos, a pane can be scoped to the
whole task or to one repo. A pane can be handed off to another agent, or
closed. The All agents view lists every agent pane in every task, with the
ones that need you first.

| id | CLI | How it reports its state | How it gets the app's tools | Resume |
|---|---|---|---|---|
| `claude` | Claude Code | hooks, passed with `--settings` | `--mcp-config` file (0600) | `--continue`, per folder |
| `copilot` | GitHub Copilot CLI | hooks, from a plugin folder | `--additional-mcp-config` file | no |
| `opencode` | OpenCode | a JS plugin posting its state | `OPENCODE_CONFIG_CONTENT` + `VILLAIN_MCP_TOKEN` | no |
| `gemini` | Gemini CLI | the mark in its window title | `.gemini/settings.json` in the app's own folders | no |

Each of them can also run as a conversation the app draws, over ACP (§20).

- **PANE-1** Only a CLI that can report its own state MUST be offered.
  Output alone cannot tell working from idle: Claude Code repaints every
  few seconds while idle. CLIs dropped for this are in git history.
- **PANE-2** At most 32 panes run at once (`MAX_PANES`), however many
  spawns race. It is a ceiling, not a setting.
- **PANE-3** A pane MUST start in an existing folder. A missing one is an
  error, never a silent fallback to `$HOME`, where `--continue` resumes the
  wrong conversation.
- **PANE-4** A pane's environment MUST be the user's login shell's
  (PATH, etc.), minus this app's own `VILLAIN_*` values and the
  `CLAUDE_CODE_*` markers of a Claude Code session the app was opened
  from (with them, Claude saves no transcript). It adds
  `VILLAIN_PANE` and the hook URL and token for the pane.
- **PANE-5** Stopping MUST be graceful then certain. An agent gets SIGTERM
  (a shell SIGHUP), up to 5 seconds (2 when a whole task is being
  deleted) to save its transcript, then SIGKILL to its process group.
  Quitting the app does the same for every pane, however it is quit:
  Cmd+Q, the Dock, closing the window, logging out.
- **PANE-6** A pane stopped on purpose (Stop, handoff, close) MUST NOT be
  reported as having exited with an error, nor posted to Slack as
  finished.
- **PANE-7** The open panes MUST come back when the app reopens, when
  "Put terminals back" is on. Agents resume their conversation where the
  CLI can. At most 12 come back at launch (`RESTORE_LIMIT`), agents before
  shells, since a shell has no conversation to lose. A pane over the limit
  MUST NOT be forgotten: it waits, listed with Reopen and Forget (a chat
  in the Chat view, a task's pane in its Terminals tab) until the user
  picks one, and the user is told how many wait. A waiting pane is never
  reopened by a later launch on its own. Restoring used to put back the
  first 12 in the order they were opened and forget the rest, so the
  newest were the ones lost: a chat with a day's work in it went, at a
  restart with 12 agents and 2 shells open. Two agents in one folder never
  both resume the same conversation. A pane that fails to come back three launches in a row is
  forgotten. Only a conversation the CLI would continue counts: Claude
  Code's one-shot runs (`claude -p`, the app's own PR description drafts)
  do not. Counted, a draft in the task folder had an agent restarted there,
  where Claude found "No conversation found to continue", while its real
  conversation was in the repo's folder.
- **PANE-8** A terminal MUST keep 256KB of scrollback. One coming back on
  screen gets exactly what it missed, by byte position. Keystrokes echo
  immediately, even while the pane is printing heavily. Output is batched
  to about 30 frames a second otherwise.
- **PANE-9** Handoff moves the work, not the conversation: no CLI can
  resume another's session. It MUST give the new agent a briefing: the task prompt,
  the commits and changed files per repo since the branch point, and the
  last 120 lines of the old pane. Then it stops the old one.
- **PANE-10** Claude Code MUST NOT stop at its "trust this folder?"
  question for folders the app created, when "Trust the folders this app
  creates" is on. The app records trust in `~/.claude.json` for those
  folders only, and nowhere else. It never creates that file, and never
  rewrites one it cannot parse. It keeps the file's permissions and
  symlink, and does not overwrite a change Claude made meanwhile.
- **PANE-11** Nothing the app types into an agent MUST be longer than 512
  bytes (`pty::MAX_TYPED`). A longer hand-off (conflicts, review
  comments, a PR description request, an opening prompt a CLI takes
  typed) is written whole to a file in the task folder, and only a
  pointer to it is typed. macOS throws away a typed line much over 1 KB
  while the program is not reading in raw mode: a rebase hand-off reached
  Claude Code as its last 68 bytes. Longer is refused, never cut. An agent
  over ACP is sent it whole instead (ACP-4).
- **PANE-12** An agent pane MUST be called by what its conversation is
  about once the CLI names it, in its tab, the chat list, All agents and
  the pickers that send it something. The name is the CLI's own, read from
  its window title (Claude Code's `✳ <topic>`), never generated by the app.
  Until then, and for a CLI that names nothing, it is the agent's name.
  Its scope (`· repo`) stays. A resumed conversation names itself again as
  it starts, so nothing is saved.
- **PANE-13** An agent in a task that has a ticket MUST find the ticket in
  the task folder's context file (`CLAUDE.md`/`AGENTS.md`): key, title,
  type, priority, labels, components, epic, link and description. The
  opening prompt carries it only into the first conversation, and only
  until the CLI compacts it. An agent started with a prompt of its own,
  or brought back at launch, had only the link, and spent its first calls
  fetching what the app already had. The ticket is saved as `TICKET.md`
  in the task folder whenever the app fetches it for the task (Start
  work, the launch dialog's opening
  prompt), and the context file is built from that. The description is
  quoted line by line and introduced as written by whoever filed it: the
  context file is read as instructions, and a ticket's text is someone
  else's. A description past 8 KB is cut, and says so.
- **PANE-14** An agent pane MUST restart in place on its own
  conversation, for a CLI that can resume (Claude Code): Restart, on its
  tab and its entry in the chat list, stops it as closing does (PANE-5,
  PANE-6) and starts the same CLI in the same folder, in the same place
  in the list. That is how a CLI that updated itself gets to run its new
  version. An agent that has exited offers the same, as Resume, above its
  terminal. The conversation is the one the CLI last said it was in (a
  Claude Code hook's `session_id`, resumed with `--resume`). Without that,
  it is the folder's newest (`--continue`), and Restart refuses while
  another open agent of the same CLI is in that folder, since the newest
  could be the other's. Before this, an agent that exited said "Resume
  this session with: claude --resume …" and nothing in the app could.
- **PANE-15** Text in any terminal MUST be selectable and copied by ⌘C,
  including while the agent has the mouse. Claude Code's fullscreen mode,
  OpenCode and Copilot turn on mouse reporting, and then a drag is theirs:
  ⌥-drag selects in the terminal instead. The selection stays until a
  click, a key or a scroll, which go to the agent as before. Before this,
  nothing could select there, and a selection made anyway was gone with
  the next movement of the mouse, which xterm reports to the agent as
  input.

Code: `pty.rs`, `agents.rs`, `commands/panes.rs` (`restart_pane`),
`Terminals.tsx`, `Terminal.tsx`, `AgentsView.tsx`, `PaneNotice.tsx`.

Known gaps:
- Resume, pre-trust, and drafting PR and ticket descriptions are Claude
  only. Drafting runs `claude` whichever agent the task uses.
- Gemini gets the app's tools and task context only in the app's own
  folders (a task folder, a chat room), not when started inside one repo.

## 4. Agent state and "needs you"

Every agent pane has a state, shown as a coloured dot everywhere it
appears:

| State | Meaning | Counts as "needs you" |
|---|---|---|
| working | busy on a turn | no |
| asking | waiting on an answer: a permission prompt, a question | yes |
| done | finished a turn you have not looked at yet | yes (not for a chat) |
| idle | nothing to do, or done and seen | no |

A pane can also carry a **notice**: `usage_limit` (the CLI said its plan's
limit is reached) or `trust_prompt`. A notice counts as needing you, and
shows as a banner above the terminals.

- **STATE-1** State MUST come from what the CLI reports (hooks, plugin,
  title, the protocol for an agent over ACP: ACP-2), never from resize,
  refresh or polling. Output timing decides only
  for a CLI that reports nothing, and not right after the app sent the pane
  something.
- **STATE-2** Esc or Ctrl-C at a question or during a turn MUST make the
  pane idle, since no Stop hook runs for an interrupted turn. Answering a
  question makes it working.
- **STATE-3** Done becomes idle once the pane has been on screen. A pane
  on a loop reads as working between its turns while the loop runs, and as
  done again when the loop hands back (LOOP-8).
- **STATE-4** Every view MUST agree: the sidebar, the task's pane bar, All
  agents, Chat and the "N need you" count use `paneState` / `needsYou` in
  `lib/derive.ts`, and the dock count uses the same rule in `attention.rs`.
- **STATE-5** A state change MUST reach the UI as an event (`pty:activity`)
  as it happens, not at the next poll.
- **STATE-6** A usage-limit notice MUST NOT fire on prose that merely
  mentions limits. It clears when the agent next reports working.
- **STATE-7** With **Keep awake** on (the cup in the top bar), the Mac
  MUST NOT idle-sleep while at least one agent, in a task or a chat, is
  working, and MUST be free to sleep again within one attention pass (5 s)
  of the last one stopping. Asking, done, idle and shells do not count. The
  display still sleeps, and closing the lid still sleeps the Mac. It is held
  by `caffeinate -i -w <the app's pid>`, so it ends with the app, crash or
  not. The cup is outlined while on, and coloured while it is holding.
  With a phone's way in open, any running agent holds it (PHONE-8).

Code: `pty.rs` (`PaneMeta::state`), `mcp.rs` (`/hook`), `agents.rs`
(`hook_activity`), `store.ts`, `attention.rs`, `awake.rs`.

## 5. Chat

The Chat view runs agents that belong to no task: for questions about the
tickets, the repos, what is in flight. Each chat has its own room folder
(`<worktree root>/_chat/<id>`). The room's context file lists the connected
integrations, the app's tools, the repos and the tasks in flight.

- **CHAT-1** Each chat MUST get its own folder, since the CLIs key their
  history by folder.
- **CHAT-2** A chat that is done MUST NOT count as needing you. One that is
  asking does. Its banners open the Chat view.
- **CHAT-3** A task a chat's agent created (`start_work`, `create_task`)
  MUST say so in its Terminals tab, with the chat's name and state and a
  way to open it, for as long as that chat is open. The agent goes on
  working on the task from the chat, so its pane is never one of the
  task's: without this, the task read "No panes yet" while the work was
  being done. The chat is known by its room folder, which outlives the
  pane's id across a restart. Which pane called comes from `X-Villain-Pane`,
  filled in by each CLI from `VILLAIN_PANE`. It is not a credential: an
  agent could name another pane, and all that would do is attach this link.

Code: `ChatView.tsx`, `ChatLink.tsx`, `commands/panes.rs` (`open_chat`,
`write_chat_context`), `mcp.rs` (`caller_chat`).

Known gaps:
- The room's "tasks in flight" list is written when the chat opens and at
  startup, so it goes stale while a chat runs.

## 6. Diff, review notes, commit

The Diff tab shows the task's changes across all its repos. There are two
scopes: Uncommitted (against HEAD) and Whole branch (against the branch
point). Whole branch can also show one commit at a time. Clicking a line
number leaves a note on it. Review asks a fresh model to read the branch
and leave notes of its own. The notes go to a running agent, or start a
new one. Commit commits every repo with changes, with one message.

- **DIFF-1** The file list and patches MUST come from git as the user's
  config would not change them. That means `--no-ext-diff`, untracked files
  always listed, and renames and non-ASCII paths kept (`-z`).
- **DIFF-2** Untracked files MUST show as all-additions patches. Binaries
  and files over 4MB are listed but never read.
- **DIFF-3** Whole branch MUST measure from the recorded branch point
  (TASK-3, moved by UPD-6), so after a merge it shows only what has not
  landed. The commit picker follows the first parent, so a merge of the
  base does not list the base's history.
- **DIFF-4** Commit MUST skip clean repos, and refuse in a repo mid-merge or
  mid-rebase. A failing commit hook keeps the dialog open with its output.
- **DIFF-5** A note sent to an agent at the task folder MUST name its repo
  (`api/src/auth.ts:42`), so it is never ambiguous which repo is meant.
- **DIFF-6** Review MUST run a fresh one-shot model over the task's whole
  branch in every repo: its commits, the diff as PR-4 gathers it, the
  saved ticket (as data, not instructions). It runs through Claude Code on the model chosen in
  Settings (Sonnet by default), may read the repositories, and may not
  write, run or fetch anything. Its findings come back as notes on lines,
  each a bug, a risk or a nit, and each a suggestion until kept: only
  your notes and kept findings are sent. A finding naming no file or line
  of the task is dropped, and one on a line the patch does not draw is
  listed above it with its line number. A finished run opens the first
  finding still to keep or drop, and the count of them goes to the next;
  a file with notes is marked in the tree. Found on a file not open, a
  finding was a count with nothing to see. A new run replaces the last one's
  findings that were not kept. An answer that is not findings is an
  error, never a clean review. Notes are held per task while the app
  runs: kept by the tab, they were lost by switching to another, and a
  run takes a minute or more.
- **DIFF-7** The file list MUST fold: each folder, and, in a task with
  more than one changed repo, each repo under its name. A fold belongs to
  its repo, so folding `src` in one repo leaves another repo's `src` open.
- **DIFF-8** A note MUST be able to say what the agent needs: any note,
  the reviewer's findings included, can be edited, and a finding edited is
  yours and kept. "Suggest a change" adds a suggestion block holding the
  line's code, selected, so typing replaces it with what it should read. A
  suggestion block is shown as a suggested change wherever notes and GitHub
  comments are drawn. A removed line can be commented on, numbered in the
  old version of the file; shift-clicking a second line number on the same
  side stretches the note being written over every line between. Both go
  to an agent as `file:12-16` or `file:3 (removed)`, with the lines they
  read.

Code: `commands/diff.rs`, `commands/reviewer.rs`, `git.rs`, `DiffView.tsx`,
`DiffNote.tsx`.

Known gaps:
- Notes and findings do not survive a restart of the app.
- The reviewer runs on Claude Code only, whichever agent CLI did the work.
- A run cannot be stopped once started, and has no time limit of its own.

## 7. Update from base

Brings each repo's branch up to date with its base, by merge or by rebase,
each repo its own team's way.

- **UPD-1** It MUST refuse when tracked files have uncommitted edits.
  Untracked files are fine. It also refuses when the worktree is on another
  branch, or already mid-update.
- **UPD-2** Merge MUST say `--ff` explicitly, since `merge.ff = only` in the
  user's config broke every merge. Rebase overrides `autoSquash`,
  `autoStash` and `updateRefs` for the same reason.
- **UPD-3** Rebase MUST refuse when the remote branch has commits the local
  one lacks, unless the remote tip is exactly the recorded lease. Otherwise
  someone else's pushed work would be dropped.
- **UPD-4** After a rebase, the remote tip is recorded as the push lease.
  The next push uses `--force-with-lease=<branch>:<that sha>`, and clears
  the lease.
- **UPD-5** Conflicts MUST be left in place, to hand to an agent (who is
  told not to push, and how to continue) or to abandon. Abandoning restores
  the branch point as it was. Any other failure is aborted at once, so a
  worktree is never left half updated.
- **UPD-6** The branch point moves to what was merged in, or rebased onto,
  and to the head of a pull request of the branch once it has merged, as
  soon as the PR watch or opening PRs sees it. Only forward: only to a
  commit the branch grew from and that has the current point in it, never
  during an unfinished update. A base merged back in after the PR already
  holds its work, and moving back would count the base's work again.
  Measured from where the branch was cut, merged work stayed in the Diff
  view, the panel's counts, a follow-up PR's description and the agent's
  history, and opening PRs opened a second PR for it.
- **UPD-7** Each repo MUST start on its own way of updating: what it is set
  to in Repos, or the way it was last updated; else a guess, in the app's
  copy, from the first of these that says anything:
  - how its recent branches on origin (the last 90 days) took the base in:
    a merge of the base, or commits rewritten with none, whichever is
    more common over three or more branches. A branch made mostly of
    merges is a release branch and does not count;
  - how pull requests land on the default branch: merge commits mean
    merge, a straight line of commits as written means rebase. Squashed
    pull requests say nothing about how the branch was kept up to date:
    taken for "merge", they guessed wrong for two frontend repos that
    rebase;
  - the way the other repos in its group go, counting only those with a
    choice or a guess of their own, since a group is usually one team;

  else merge. The guess is said to be one, with its reason, and never
  overrides a choice. One app-wide mode, the last used, was wrong as soon
  as a task spanned a team that merges and one that rebases.
- **UPD-8** Each repo's base MUST be changeable in the dialog, from its
  origin branches or typed, and a base not among them at the last fetch is
  said to be. It is the same base the next pull request is opened against,
  and changing it drops the branch point (DIFF-3 then measures from the
  merge base). Only the Open pull request dialog had the field, and that is
  shut once every repo has a PR: a stacked base merged and deleted upstream
  could not be moved off.

Code: `commands/diff.rs` (`update_from_base`), `git.rs`,
`git/upkeep.rs` (`update_style`), `commands/landed.rs` (UPD-6 after a merge), `UpdateFromBase.tsx`.

Known gaps:
- The guess reads only history. The site's own rules for the base branch
  (GitHub's "require linear history", a ruleset's allowed merge methods)
  are not asked, though they would be firmer than a guess.

## 8. Pull requests

The Pull requests tab shows, per repo, the open PR, or an earlier one if
none is open. It shows checks, the latest review per reviewer, the verdict,
and how far behind the base it is. From here: push everything, open PRs,
retarget a PR onto the checkout's base or take the PR's base for the
checkout (GitHub moves a PR itself when its base is merged and deleted),
send feedback to an agent, and finish a merged task.

- **PR-1** Opening PRs MUST push each repo that has commits, open one PR per
  repo against that checkout's base (draft by default), and reuse a PR that
  is already open. A repo with only uncommitted changes is refused: the app
  never commits for you here. A merged PR first moves the branch point up
  to what it landed (UPD-6), so a repo with nothing since is skipped, not
  opened again. Counted from where the branch was cut, a repo merged while
  another was still in review got a second PR for the same work.
  In the dialog, every repo that would get a new PR has a tick, all on to
  start with; one ticked off is neither pushed nor opened. A repo whose PR
  is already open has no tick: it is pushed and listed with the rest.
- **PR-2** PRs this call opened MUST be linked on the Jira ticket as a
  comment, and posted to Slack if that is on. When the call opens a PR and
  the task has more than one open, every one of them MUST list the others
  in its description, in a marked section the app rewrites; the rest of
  the description is left as written. A failed link is reported, not
  swallowed.
- **PR-3** Push MUST go through `git::push`, with the lease when there is
  one (UPD-4). Never `--force`. Pushes run in every repo at once.
- **PR-4** The description can be drafted. A one-shot model run gets the
  commits, the stat and the diff (lockfiles left out, 60k characters at
  most), and the spec's requirements with their check (SPEC-16), and
  streams its answer into the field. Failing that, a running
  agent is asked to write `PR_DESCRIPTION.md` in the task folder.
- **PR-5** Review threads MUST be read in full: every page, resolved and
  outdated flags kept. A verdict is each reviewer's latest decisive review.
  One "changes requested" outranks any number of approvals, and a dismissal
  clears it.
- **PR-6** Feedback to an agent is asked for from a PR's own card, with a
  count of that PR's comments and failing checks; one button for every PR
  gave no way to tell whose they were. The picker shows that PR, and a task
  with several switches between them, each saying how many of its items
  are picked. Everything picked, on any of them, is sent in one go, and the
  send button says from how many PRs.
  It MUST pre-select only what is new: not
  resolved, not sent before, not from a bot, not your own, not outdated.
  The feedback is written to `PR_FEEDBACK.md` in the task folder, never in
  a worktree, and a pointer is typed into the agent. It is not told to keep
  off GitHub: whether it replies or resolves threads is the user's call,
  and told never to, it refused when asked to.
- **PR-7** Failing GitHub Actions checks MUST come with the end of the
  job's log, trimmed to the error.
- **PR-8** Every task's PRs are refreshed every 90 seconds (180 when idle)
  while the window is in front, timed from the last sweep: switching views,
  going idle and coming back never put one off, and coming back to the
  window when one is due runs it at once. (The timer used to restart on
  each of those, and in ordinary use never fired.) The first sweep is silent. After it, a new
  approval, request for changes, new comment or merge is announced as a
  toast.
- **PR-9** A late answer MUST NOT overwrite a newer one. A sweep that
  started before a PR was opened does not put back "no PR".
- **PR-10** The feedback picker MUST show each item in full, with no
  click to expand it: every comment of a thread, and a check's report and
  log. Markdown is shown as GitHub shows it (headings, emphasis, code,
  lists, tables, links), built from elements and never from HTML, since
  the text is someone else's: raw tags keep only their text, a link opens
  in the browser only for http(s) and mailto, and images are not loaded.
  Each item is a box of its own, marked on its edge when picked: as rows
  split by a hairline, a long review ran into the next and there was no
  telling where one ended. Clicking an item's heading picks it; its text
  can be selected.
  A review thread shows the code it is on above its comments, as GitHub
  does: the lines it spans, or the line and the three above it, cut from
  the hunk it was written against (so an outdated thread still shows the
  code it meant). Its heading names the range, `file:16-20`, and the same
  code goes to the agent.
  A resolved thread is the one exception to "in full": it is listed last
  in its repo, shut to its heading with a `resolved` chip, and opens with
  one click on its arrow. It is never picked for you, and "All" leaves it
  out. One ticked on purpose goes to the agent marked as resolved, with a
  warning not to undo what was settled.
- **PR-11** Open pull requests MUST show what it is about to push before
  it does, per repo: the commits origin does not have yet (from the
  branch on origin, or the branch point when there is none or it was
  rebased), and in their added lines what is usually there by accident:
  debug output (`console.log`, `dbg!`, `debugger`, `binding.pry`, …), a
  focused test (`.only`, `fit`), a conflict marker, a new TODO or FIXME.
  Prose files (`.md`, `.txt`, …) are not read for these, nor is anything
  uncommitted, which a push does not send. It says whether the reviewer
  (DIFF-6) has read these commits, findings still to keep or drop, and
  notes not yet sent, and offers Review there. It is advice, never a
  gate: with anything flagged, the button reads "Open anyway". A push used
  to be the first time anyone saw the branch whole.

Code: `commands/github.rs`, `commands/open_prs.rs`, `commands/outgoing.rs`, `git/outgoing.rs`, `Outgoing.tsx`, `commands/landed.rs`, `integrations/github.rs`, `PrPanel.tsx`,
`PrFeedback.tsx`, `Markdown.tsx`, `lib/markdown.ts`.

Known gaps:
- Only the first 100 check runs of a commit are read.
- Legacy commit statuses (Jenkins, older CircleCI) are not read at all.
- A failed checks or reviews request reads as "no checks" / "no verdict"
  rather than as an error.
- Two feedback sends for one task seconds apart overwrite each other's
  `PR_FEEDBACK.md`.
- There is no rate-limit handling beyond pacing.
- Copilot's title, severity badge and "suggested changeset" on a review
  comment are not shown. GitHub does not return them with the comment,
  and the suggestion is not in its body the way a reviewer's
  ```` ```suggestion ```` block is.

## 9. Tickets

The Tickets view has two tabs. **Mine** is your assigned, not-done tickets,
grouped by epic, with a filter and type toggles. **Find work** searches
beyond them: not mine / unassigned / anyone, done included or not, by type,
by text, or by key. A ticket opens its task if there is one; otherwise it
offers Start work.

- **TKT-1** Start work MUST create the task for the ticket. It suggests
  repos from what was used before for its epic, then its project, and says
  which it was ("preselected from the last task under ACME-100"), so a
  stale guess is visible. It picks
  a base, optionally starts an agent with the ticket as the prompt (or,
  with "Write a spec first", leaves that to the Spec tab, SPEC-18), and
  moves the ticket where its project's "work starts →" says, beside the
  other stages of Tickets follow the work (TKT-8): to the first
  in-progress (`indeterminate`) status its workflow allows, unless it is
  in that category already (the default); to a status chosen there,
  unless it is there already, out of done too; or nowhere. A project
  with nothing chosen goes as the switch in General that came before
  said, on or off. That switch was out of sight of the other stages, and
  a ticket moved to the first in-progress status of a workflow with
  several could only be left or taken there.
- **TKT-2** Nothing MUST depend on a site's names. Epics are types at
  hierarchy level 1 or above. "In progress" and "done" are status
  categories. The Epic Link field is found by its schema. The default type
  is the level-0 type called "Task" if there is one, else the first.
- **TKT-3** Filing an issue MUST ask Jira what the project requires
  (`createmeta`), offer a choice for each required field with a fixed set
  of values, and name any it cannot fill.
- **TKT-4** Text typed into a search MUST be quoted as a JQL string.
  Project keys are always quoted (a key can be a reserved word, like `IT`).
- **TKT-5** Any key or id from Jira that goes into a URL path MUST be
  validated first. A ticket's text is written by anyone.
- **TKT-6** "Improve description" MUST stream a rewrite of the ticket or
  epic description into the field, for the user to accept or edit.
- **TKT-7** Your ticket list is re-read every 3 minutes while the window is
  in front. While it is away, the backend looks for itself (NOTE-3).
- **TKT-8** A task's ticket MUST follow its pull requests: to the project's
  review status once one of them is ready for review (not a draft), and to
  its after-merge status once every one has merged, with nothing left
  unmerged or uncommitted. Both statuses are chosen per Jira project in
  Settings, from the project's own statuses: In Progress, Review and
  Testing all share Jira's "in progress" category, so the app is told,
  never guessing by name. Until one is chosen the app says so, once per
  project and stage, and moves nothing. Each stage moves the ticket once per
  task (`Task.ticket_stage`), so a ticket moved by hand afterwards stays
  where it was put; a review never takes a ticket out of done. A workflow
  with no transition to the chosen status is said once, not retried. It
  runs with the PR sweep (PR-8). Before this, tickets sat In Progress with
  their pull requests merged weeks before.
- **TKT-9** Deleting a task MUST show its ticket's status and offer its
  transitions. It leaves the ticket as it is by default, since a deleted
  task is as often abandoned as done, and pre-selects the after-merge
  status when a pull request of the task has merged. The ticket moves only
  once the task is gone.
- **TKT-10** The task header MUST show its ticket's status, from your ticket
  list, so a ticket out of step with the work is seen where the work is.
- **TKT-11** Every ticket card MUST say how long ago the ticket was created,
  in its largest whole unit ("5m", "3h", "2d", "1w", "4mo", "2y"), with the
  full date on hover.
- **TKT-12** In Find work, a key MUST be looked up rather than searched for.
  A bare number is a key in the project set in Settings ("4826" is
  `ACME-4826`), since Jira's text search does not look at keys. If no
  ticket has that key, or the filters hide it, the number is searched for
  as text instead. Without a project set, a number is only text.

Code: `commands/jira.rs`, `commands/ticket_flow.rs`, `integrations/jira.rs`,
`integrations/jira/flow.rs`, `components/tickets/`, `TicketFlow.tsx`,
`sidebar/DeleteTask.tsx`.

Known gaps:
- Jira Cloud only: email and API token, REST v3. No Data Center or Server
  personal access tokens.
- Rich-text mentions, status and date nodes, and link targets are dropped
  when a description is read.
- Start work moves the ticket without the confirmation the MCP
  `jira_transition` tool asks for. The tool's description does not say so.
- Tickets follow the work only while the app runs with its window in
  front, since that is when the PR sweep runs. A PR merged overnight moves
  its ticket the next morning.
- The task header shows a ticket's status only when it is in your ticket
  list (assigned to you, not done).
- A move that needs two transitions (a workflow with no direct way from
  In Progress to the after-merge status) is reported, not made.

## 10. Reviews

The Reviews view has two tabs, one for each side of a review. "To review"
lists open PRs waiting on your review, excluding your own, and, when a
review team is set, those waiting on that team. "Opened by you" lists the
open PRs you opened, and where each one stands. The tab last used is kept;
a banner or message about a review request opens "To review". The top
bar's Reviews tab carries both counts, as its tabs show them, `1 · 7`: it
used to show only the first, so seven PRs of yours read as none.

- **REV-1** A failed team lookup MUST NOT hide your own list. It shows as
  an error on the team section.
- **REV-2** A bare team slug (`@fe`) MUST be resolved from the teams you are
  on, which needs the `read:org` scope. `org/slug` needs no lookup.
- **REV-3** PRs already in your own list MUST NOT repeat in the team's.
- **REV-4** "Opened by you" MUST list every open PR you authored, in any
  repo, whether or not a task made it, and say for each: draft, its checks
  as GitHub rolls them up (check runs and commit statuses), the review
  decision (or, where the repo requires none, the latest reviews), who is
  still asked to review, how many threads are unresolved, and a conflict
  with its base. One that belongs to a task links to that task's Pull
  requests tab.
- **REV-5** A failed lookup of your PRs MUST NOT hide the review lists, nor
  they it. It shows as an error on its own section.
- **REV-6** It reads the 50 most recently updated and the first 100
  threads of each, and MUST say so when there are more (`50+`, `100+`).
- **REV-7** One of your PRs MUST lead to its work: a click opens its task's
  Pull requests tab, and GitHub is a button on the card. One with no task
  offers "Start task", which makes one in the registered repo whose origin
  is the PR's (`owner/name`), on the PR's own branch (TASK-3) and against
  its base, named after its title, with a Jira key in the branch or title
  linked as the ticket when Jira is connected. A task already on that
  branch is opened instead, with the repo added if it lacked it. A PR in a
  repo that is not registered says so, and makes nothing.
- **REV-8** "Opened by you" MUST group your PRs by whose move it is, in
  this order: needs you, ready to merge, checks running, waiting on
  review, drafts. A group with nothing in it is not shown. Each group can
  be folded, and keeps the most recently updated first. A flat list of a
  dozen PRs hid the two that needed you among the drafts.
- **REV-9** Every PR in "To review" MUST say what it asks of you: its size
  (lines added and removed, files), its checks as GitHub rolls them up,
  whether it comes from a fork, and your own standing: not reviewed yet,
  your latest review (approved, changes asked for, commented, dismissed),
  or new commits since your review, when its head has moved past the
  commit your latest review was of. A pending review of yours is a draft,
  not a review given. The list is the 100 most recently updated, and says
  so when there are more. Before this, every card was a title and an
  author: a one-line fix looked like a rewrite, and a PR you had approved
  before its author pushed again looked like one you had already done.
- **REV-10** A PR in "To review" MUST open on GitHub, where it is
  reviewed. Reviewing it here (a checkout of its own, Claude's reading
  plan and findings beside its diff, and the review posted from the app)
  shipped in 0.5.0 and was taken out: the page was the Diff tab for your
  own work with a review laid over it, and it did not read as a review. A
  review checkout left from 0.5.0 lists as an ordinary task, named
  `Review: <title>`, to be deleted. It is still never pushed nor opened as
  a pull request, and its ticket never moves.

Code: `commands/github.rs` (`review_queue`, `github_review_queue`),
`integrations/github/authored.rs`, `integrations/github/requested.rs`,
`commands/pr_task.rs`, `ReviewsView.tsx`.

## 11. Notifications

| What | When | Where |
|---|---|---|
| Dock count | agents that need you (STATE table) | `attention.rs`, every 5s |
| Banner: agent | an agent starts needing you, or exits on its own, while the window is not focused | `attention.rs` |
| Banner: review, ticket | a new review request or assigned ticket, while the window is not focused | `news.rs`, every 3 min while away |
| Toast | PR approved, changes requested, new comments, merged; a pane that exited; errors | `Watchers.tsx` |
| Message center | all of the above but Slack and the dock, kept | `messages.rs`, `MessageCenter.tsx` |
| Slack | an agent finished, PRs opened, an agent's own post | `commands/slack.rs` |

- **NOTE-1** Banners MUST be sent by the backend. A hidden webview's timers
  are the ones macOS throttles, so banners from the UI failed exactly when
  they were needed.
- **NOTE-2** The first look MUST be a snapshot, not news. Opening the app
  does not banner everything already waiting. A failed lookup forgets
  nothing, so the next success does not announce it all again.
- **NOTE-3** More than three at once MUST become one banner. The same pane
  is not announced again within two minutes.
- **NOTE-4** Clicking a banner or a toast MUST open exactly what it is
  about, by one route (`lib/target.ts`): an agent's opens its task with that
  pane selected (a chat's, Chat with that chat); pull request news opens the
  task's Pull requests tab; a ticket opens Tickets on that key; a review
  request opens Reviews with that pull request highlighted. What has gone
  since falls back: a pane (every restart gives panes new ids) to its task,
  a task to the Work overview. A toast about nothing in particular only
  closes. A banner also brings the window forward, minimized or not.
- **NOTE-5** Slack posts MUST pass the backend's switches: "Send anything"
  first, then one per kind. Nothing routes around them. A webhook URL is a
  secret and never appears in an error.

### Message center

The bell left of Settings. Toasts go and banners pile up in Notification
Center; this keeps what the app told you, newest first.

- **MSG-1** The message center MUST keep every piece of news: an agent
  asking, finished or exited on its own (with a trust prompt or a usage
  limit), a new review request, a new ticket, a ticket the work moved, a
  pull request approved, sent back, commented on or merged, a notice from
  the app, and every error shown as a toast. Feedback on something you just
  did ("Copied …") MUST NOT be kept.
- **MSG-2** News MUST be recorded whether or not the window is focused and
  whether or not its banner is switched on. The switches decide banners and
  the dock count, nothing else. The first look is still a snapshot (NOTE-2).
- **MSG-3** Each piece of news MUST be recorded once, by one source: agents
  by `attention.rs`, reviews and tickets by `news.rs`, notices by
  `push_notice`; pull request news, ticket moves and errors by the UI's
  toast (`add_message`), which is the only place they are seen. A toast of
  something the backend recorded does not record it again. The same unread
  message again within ten minutes MUST count on the row already there
  (×N) rather than add another.
- **MSG-4** The log MUST survive a restart: `messages.json` beside
  `config.json`, the newest 200, written whole through a temp file and a
  rename, never on the main thread or the async runtime. An unreadable file
  is kept as `messages.json.unreadable`, and one entry this build cannot
  read is dropped on its own.
- **MSG-5** The bell MUST show how many are unread, and list them newest
  first with when and how often each was said. A click on one opens it by
  NOTE-4's route and marks every unread message about the same thing read.
  One about nothing in particular is only marked read. "Mark all read" and
  "Clear" act on the whole log; Clear asks first.

Known gaps:
- "An agent finished" is posted to Slack by the UI on `pty:exit`, so it
  depends on the webview being awake.

## 12. Tools for agents (MCP)

The app runs an MCP server on `127.0.0.1` (a random port, a new bearer
token each launch) and gives it to every agent it starts. Agents use the
app's own Jira, GitHub and Slack connections through it, and never hold
credentials themselves.

| Tool | Does | Needs `confirm` |
|---|---|---|
| `list_tasks` | tasks in flight, with worktrees and dirty counts | |
| `list_repos` | registered repositories | |
| `task_diff` | changed files across a task, against the branch point | |
| `jira_search` | JQL search, 1–500 results | |
| `jira_get_issue` | one issue in full | |
| `jira_issue_types` | types with hierarchy levels | |
| `jira_create_fields` | what a project requires for a type | |
| `jira_create_issue` | file an issue, required `fields` included | |
| `jira_comment` | comment on an issue | |
| `jira_transition` | list transitions, or move an issue | to move |
| `slack_post` | post to the configured channel | |
| `slack_cleanup` | delete this app's own posts (`dry_run` to list) | to delete |
| `slack_delete` | delete specific posts by permalink | yes |
| `slack_diagnose` | the token's scopes and channel | |
| `start_work` | a task from a ticket, optionally with an agent | |
| `handoff_prompt` | a pane's handoff briefing (needs "Let agents read terminal output") | |
| `list_panes` | panes and their states | |
| `pane_output` | a pane's recent output (needs "Let agents read terminal output") | |
| `task_prs` | a task's PRs, checks and reviews | |
| `add_repo` | add a repo to a task, telling its agents | |
| `forget_repo` | unregister a repo (never deletes files) | yes |
| `create_task` | a task with no ticket | |
| `open_prs` | push and open PRs for a task | yes |
| `repo_notes` | a repository's notes, with what changed since each was checked (MEM-5) | |
| `remember` | keep a lasting fact about a repository, after asking the user (MEM-1) | yes |
| `check_note` | say a note still holds, as of now | |
| `forget_note` | remove a note that no longer holds, with a reason for the message center | yes |
| `spec_task` | tick (or untick) a numbered step of a repository's spec `tasks.md` (SPEC-13) | |
| `browser_navigate` | open a page in the task's browser tab (BRW-3), and get its outline | |
| `browser_back` | back one page | |
| `browser_snapshot` | the page as an outline, each element it can act on numbered | |
| `browser_click` | click an element, as real mouse input | |
| `browser_hover` | move the mouse over an element | |
| `browser_type` | type into a field | |
| `browser_press_key` | press a key, with modifiers | |
| `browser_select` | choose options in a list | |
| `browser_scroll` | scroll the page, or the part under an element | |
| `browser_wait` | wait for text to show, or for some seconds | |
| `browser_screenshot` | an image of the tab | |
| `browser_console` | the page's console messages and uncaught errors | |
| `browser_evaluate` | run JavaScript in the page | |
| `browser_request_site` | ask the user to let agents use a site (BRW-11) | |
| `browser_dialog` | answer the page's alert, confirm or prompt (BRW-13) | |
| `browser_tabs` | the task's tabs, and which is active (BRW-16) | |
| `browser_new_tab` | open a page in a new tab (BRW-3, BRW-16) | |
| `browser_switch_tab` | make another tab the active one | |
| `browser_close_tab` | close a tab, the active one by default | |
| `browser_sign_in` | fill a saved sign-in into the page, without seeing the password (BRW-19) | |
| `browser_copy_session` | copy a sign-in from another tab on this machine into the active one (BRW-21) | |

- **MCP-1** Every request MUST carry the bearer token. Loopback is not
  authorisation.
- **MCP-2** The tools' token MUST NOT appear on any command line. It goes
  in a 0600 file, an environment variable, or an ACP agent's stdin (ACP-3). Status reports (`/hook`) use
  a separate token that can do nothing but set a pane's state.
- **MCP-3** A tool that changes shared state irreversibly MUST need
  `confirm: true`, and says so in its schema.
- **MCP-4** Reading other panes' output MUST be off by default.
- **MCP-5** A new tool MUST be added to the table above (**guard**). If
  agents should reach for it without being told, it goes in the tool list
  the chat room's context names too (`write_chat_context`).

Code: `mcp.rs`, `agents.rs` (per-CLI wiring), `commands/panes.rs`
(`plug_in`).

Known gaps:
- A task folder's `.mcp.json` is rewritten only when an agent starts there.
  A CLI run by hand in a task folder where no agent started this launch
  gets a stale token.

## 13. Settings

| Setting | Default | What it does |
|---|---|---|
| Interface scale | 100% | 80–160% |
| New reviews and tickets | on | banners while the window is away (NOTE-*); the message center keeps them either way (MSG-2) |
| Agents waiting on you | on | banners and the dock count; the message center keeps them either way (MSG-2) |
| Put terminals back when the app reopens | on | PANE-7 |
| Let agents read terminal output | **off** | MCP `pane_output`, `handoff_prompt` |
| Reviewer | Sonnet | the model Review runs on (DIFF-6) |
| Keep awake (the cup in the top bar) | off | STATE-7, PHONE-8 |
| Phone: Tailscale | off | PHONE-1, PHONE-2 |
| Phone: Home network and router VPN | off | PHONE-1, PHONE-2 |
| Phone: Let phones type into agents | off | PHONE-7 |
| Browser: sites agents may use | none: this machine's pages only | BRW-3, BRW-11 |
| Browser: saved sign-ins | none | BRW-18 |
| Tickets follow the work (Jira) | work starts: first in-progress status; review, merged: not chosen | TKT-1, TKT-8, per Jira project |
| Trust the folders this app creates | on | PANE-10 |
| Terminal text | 13px | 9–24px |
| Conversation text | 14px | 9–24px, an agent over ACP (ACP-15) |
| Task folder location | `~/.villain-worktrees` | where task folders go |
| Jira | — | site URL, email, API token, optional project key and JQL |
| GitHub | — | API URL (`…/api/v3` for Enterprise), token (`repo`; `read:org` for a bare team slug), review team |
| Slack | — | bot token (`xoxb-`, `chat:write`) or webhook URL, channel, four switches |

- **SET-1** A token field left blank MUST reuse the saved token only for
  the same site (scheme, host, port). A different site needs the token
  typed again.
- **SET-2** Disconnecting MUST delete the token from the keychain and the
  integration's settings.
- **SET-3** A Jira site or GitHub API URL MUST be `https://`, so a token
  never travels unencrypted. Plain `http://` is refused, when connecting
  and for one saved before this rule, except to this machine
  (`localhost`, `127.0.0.1`, `::1`). Slack's addresses are fixed and
  always https.
- **SET-4** A Jira token the site no longer accepts (expired, revoked)
  MUST fail every Jira call with an error that says to enter a new token.
  Jira serves such a request as an anonymous visitor's, so without this
  an empty search, or a 404 for a ticket that exists, looks like an answer.

Code: `commands/settings.rs`, `config.rs` (`UiPrefs`), `Settings.tsx`,
`integrations/mod.rs` (`require_https`).

Known gaps:
- Changing Slack's channel needs the token pasted again.

## 14. What the app writes on your machine

| Where | What |
|---|---|
| `~/Library/Application Support/eu.codevillain.villain-layer/` | `config.json`: repos, tasks, settings, saved panes. `messages.json`: the message center (MSG-4). `notes.json`: repo notes (MEM-7). At agent launch also `.mcp.json` (0600), `claude-hooks.json`, `copilot-plugin/`, `opencode-plugin.js` |
| Keychain, service `eu.codevillain.villain-layer` | one item holding every token, paired phones' included (PHONE-3), and the passwords of the browser's saved sign-ins (BRW-18) |
| `~/.villain-worktrees/` (settable) | task folders, `_chat/` rooms, and `.repos/`: the app's own copy of each repo (REPO-4) |
| a task folder | the worktrees, `AGENTS.md` and `CLAUDE.md` (task context), `TICKET.md` (PANE-13), `.spec-drafts/`: spec drafts and checks (SPEC-6, SPEC-15), `.mcp.json`, and `.gemini/settings.json`, `PR_DESCRIPTION.md`, `PR_FEEDBACK.md`, and hand-offs too long to type (`CONFLICTS.md`, `REVIEW_COMMENTS.md`, `PR_DRAFT_REQUEST.md`, `FIRST_PROMPT.md`, `CHECKS.md` (LOOP-6), PANE-11) as they come up |
| `~/.claude.json` | trust entries for the app's own folders only (PANE-10) |
| `<config folder>/browser/` | the browser's own Chrome profile (BRW-1): cookies, sign-ins and storage of the pages opened there |
| `~/Library/Logs/villain-layer/panic.log` | a crash's location and backtrace |

The dev build uses `eu.codevillain.villain-layer.dev` for its config folder
and keychain item instead.

- **DISK-1** Nothing generated MUST ever be written inside a worktree,
  where it would end up in a commit. The one exception is a spec the user
  approved (SPEC-1), which is meant to be committed.
- **DISK-2** Deleting a task MUST remove what the app generated in its
  folder, and what others leave there that goes with it (Claude Code's
  `.claude/settings.local.json`, Finder's `.DS_Store`), and then the
  folder, if it is empty. Clean up (REPO-8) removes the same from a folder
  no task uses.
- **DISK-3** `config.json` MUST be written whole, via a temp file and
  rename. A file from a newer build is copied aside before an older build
  touches it. An unreadable one is kept as `config.json.unreadable`.
- **DISK-4** The first launch under an identifier, while its config folder
  does not exist yet, MUST start from the config and tokens saved under the
  identifier the app had before (`dev.villain.layer`, and
  `dev.villain.layer.dev` for the dev build). They are copied, never moved.
  A config folder that exists adopts nothing, even without its
  `config.json`, so starting over stays started over and a disconnected
  token stays gone.

Code: `previous.rs`, `secrets.rs` (`adopt`).

---

## 15. The window

The window has no native title bar: the app's top bar takes its place,
under the traffic lights.

- **WIN-1** The window MUST move when you drag any empty part of the top
  bar, and zoom when you double-click it, as a native title bar does. Its
  tabs and buttons stay clickable. This is Tauri's `data-tauri-drag-region`
  with the `core:window:allow-start-dragging` permission; the CSS
  `-webkit-app-region` does nothing in macOS's WebKit, and a window built on
  it could not be moved at all.

Code: `App.tsx` (`TopBar`), `src-tauri/capabilities/default.json`.

## 16. Repo notes

What agents learned about a repository that holds beyond one task, kept by
the app and given to the next task on that repository. Notes are personal:
they stay on this machine and never go into the repository, whose own
`AGENTS.md`/`CLAUDE.md` is where a team's knowledge belongs. Each repo in
the Repos view has a Notes list, to read, edit, check and delete them.

- **MEM-1** A note MUST be about the repository, not a task: how to build
  or test it, a trap in its setup, where something lives, a convention its
  code keeps. An agent MUST ask the user whether it is worth remembering,
  quoting the note, before saving it, and `remember` needs `confirm: true`
  (MCP-3). A note is read into every later task on the repository as
  instructions, so one saved unasked, or planted by a ticket's text, would
  steer every agent after it.
- **MEM-2** A note MUST record when it was written, from which task and
  agent, and the commit the default branch of the app's copy was at. It
  MAY name the paths it is about.
- **MEM-3** A note MUST show how likely it is to be out of date: when it
  was last checked (being written counts), and, if it names paths, whether
  any of them changed on the default branch since the commit it was last
  checked at. Worked out when notes are read, never on a timer.
- **MEM-4** A task's context file MUST give the notes of each repository
  in it, most recently checked first, each with its age and whether its
  paths changed, up to 3 KB per repository, saying how many were left out.
  It tells agents that notes can be out of date: check one against the code
  before relying on it, then mark it checked, or ask the user to remove it.
- **MEM-5** Agents MUST be able to list a repository's notes
  (`repo_notes`), mark one checked at the current commit (`check_note`),
  and remove one with a reason (`forget_note`, `confirm: true`, after
  asking the user). A note is corrected by forgetting it and remembering
  the new one.
- **MEM-6** The Repos view MUST show each repo's notes: the text, when it
  was written and last checked, where it came from, and whether its paths
  changed since. Each can be edited (which counts as checking it), marked
  checked, or deleted, and a note can be added by hand.
- **MEM-7** Notes MUST live in `notes.json` beside `config.json`, never in
  `config.json` itself. At most 50 per repository and 500 characters per
  note; `remember` past either says so rather than dropping the oldest.
  A note belongs to the repository, not to its registration: notes are
  kept by where the repository fetches from (its `origin`, `git@host:a/b`
  and `https://host/a/b.git` alike; the clone's path when it has none), so
  removing a repo (REPO-3) keeps them, and adding it again, or a second
  clone of it, finds them. A repo's id is new each time it is added.

Code: `notes.rs`, `commands/task_context.rs`, `mcp.rs`, `ReposView.tsx`.

## 17. Phone

The tasks and their agents, from a phone, while everything keeps running
on the Mac. The app serves a page of its own to a phone that has paired
with it: every task and its agents, the ones that need you first, and an
agent's terminal, live, with a line of text and a few keys to answer it.
Settings → Phone switches on the ways in: **Tailscale** (from anywhere,
over a tailnet) and **Home network and router VPN** (the same Wi-Fi, or a
router's own VPN such as WireGuard on a GL.iNet, which puts the phone on
the home network). How to set either up is in `docs/phone.md`.

- **PHONE-1** The app MUST answer on the network only while a way in is
  on, both off by default. It is a server of its own on a fixed port (7420,
  the dev build 7421, so the address saved on a phone keeps working); the
  MCP server stays on loopback. A port in use by something else is said,
  not left silent.
- **PHONE-2** A caller MUST come from a way that is on: Tailscale's
  addresses (100.64.0.0/10) for Tailscale, the private ranges (10/8,
  172.16/12, 192.168/16) for the home network. The internet, loopback and
  IPv6 are never let in. The server listens on every IPv4 interface and
  judges the caller's address, because the Mac's own addresses come and go
  with the network it is on and with Tailscale starting.
- **PHONE-3** A phone MUST pair before it sees anything but the pairing
  page: Settings shows a six-digit code that lasts two minutes, takes five
  tries, and pairs one phone. Each phone gets a token of its own (256 bits),
  kept in the keychain and sent as a bearer header, never in a URL.
  Forgetting a phone in Settings shuts it out at once and ends its open
  streams.
- **PHONE-4** The phone's page MUST come from the app's own build
  (`phone.html`), so it is always the version the app speaks. A dev build
  serves it from `dist/`, built by `bun run build`.
- **PHONE-5** The overview MUST show every task with its panes and their
  states, the ones that need you first by the dock count's rule (STATE-4),
  and change when they do, pushed, not polled. Panes are called as in the
  window (PANE-12). A task starts closed, showing a dot per pane and how
  many need you or are working; the phone remembers which ones were
  opened. The ticket key is shown once, not again in a name that starts
  with it.
- **PHONE-6** A phone MUST follow a pane's output from its own position
  and never change the window's feed (`watched`, `sent`). It runs the
  output through a terminal at the size the window gave it and never
  resizes it, which would scramble the Mac's view. It shows that terminal's
  rows as text wrapped to the phone, without box frames and alignment
  padding (`readable.ts`): an agent's 100-odd columns fitted to a phone
  were six pixels a letter. "Screen" shows the terminal itself. Opening a
  pane on the phone is looking at it: done becomes idle (STATE-3).
- **PHONE-7** A phone MUST type only with "Let phones type into agents"
  on, and only into a running agent, never a shell. It sends a line, made
  one line and stripped of control characters, at most 512 bytes (PANE-11),
  and Enter; or one key of the key bar: Enter, Esc, Up, Down, Tab,
  Shift-Tab, Ctrl-C, 1, 2, 3. Esc and Ctrl-C end a turn as at the Mac
  (STATE-2). An agent over ACP takes the keys as ACP-10 says.
- **PHONE-8** With Keep awake on and a way in open, the Mac MUST stay
  awake while any agent runs, not only while one works: one asking for
  permission is exactly the one you would answer from the phone. Closing
  the lid still sleeps the Mac.
- **PHONE-9** The top bar MUST show a phone once a phone is paired: green,
  naming it, while a paired phone has the app open; grey otherwise, saying
  whether phone access is off. A click opens Settings → Phone.

Code: `phone/` (`net.rs` ways in, `pair.rs`, `routes.rs`, `view.rs`,
`input.rs`), `pty/feed.rs`, `commands/phone.rs`, `awake.rs`,
`PhoneSettings.tsx`, `phone.html` and `src/phone/`.

Known gaps:
- Plain HTTP. Tailscale and a router's WireGuard encrypt the way from the
  phone; on the home Wi-Fi itself, traffic between the phone and the Mac
  is readable by anything else on that network. No notifications reach the
  phone either: those need HTTPS. Slack's (§11) still do.
- The key bar sends the arrows' normal-mode codes. The agent CLIs read
  them; a program that only takes the application-mode ones would not.
- A phone cannot start, stop or hand off an agent.

## 18. Browser

Each task has tabs in a browser its agents can use: to open the dev
server, read the page, fill in a form and click through it, as a person
trying the change would. It is one Chrome for the app, run in the
background with a profile of its own, and a set of tabs in it per task,
one of them active, which the user and the task's agents share. Agents
drive it with the `browser_*` tools (§12): they read a page as an outline
of numbered elements, act on those, and get the page back as it is
afterwards.

The globe in a task's pane bar shows its browser beside the terminals (a
dot on it says the task's agents have used it). The panel has a tab strip,
an address bar, back, forward and reload, and the active tab's page, live: the user
clicks, scrolls, types and pastes in it as in any browser, to sign in or
to help an agent that is stuck. Under it is what an agent last did, and a
ring marks where it clicked.

- **BRW-1** There MUST be one browser for the app, started when it is first
  needed, with one tab per task. It is the Google Chrome (or Chromium)
  installed on the Mac, run headless with a profile of its own in the
  app's config folder: never the user's own Chrome profile, its sign-ins or
  its extensions. Without Chrome, the tools say what to install.
- **BRW-2** An agent's tools MUST act on its own task's tab, found from the
  pane that calls (`X-Villain-Pane`). A chat's agent has none. The header
  is not a credential (CHAT-3): an agent could name another task's pane and
  drive that task's tab, which is no more than it could do by asking.
- **BRW-3** Agents MUST use only pages on this machine (`localhost`,
  `*.localhost`, `127.0.0.1`, `[::1]`) and on the sites the user allowed
  (Settings → Browser, the panel's Allow, or a request answered, BRW-11),
  over `http` or `https`. A site covers its subdomains, and is kept as its
  host whatever was typed. Opening any other page is refused, and so is
  reading or acting on a tab that a link or a redirect took elsewhere. A
  page's text is written by whoever runs the site, and an agent reads it
  as it reads a ticket: as something that may tell it what to do.
- **BRW-4** An action MUST be real input: mouse events at the element's
  middle, scrolled into view, and keys as key events, so a page cannot tell
  an agent's click from a person's. Each action waits for a page load it
  started (up to 10 seconds) and answers with the page as it is then.
- **BRW-5** A password field's value MUST NOT appear in an outline. The
  tools tell agents never to type the user's secrets, and to ask the user
  to sign in themselves.
- **BRW-9** The panel MUST show the task's tab as it is, and the user's
  own input MUST reach it as real input: mouse buttons, moves and the
  wheel, keys, paste, dictation and an input method's text. The page is
  laid out at the panel's size, so the user and the agents see the same
  page. Frames are sent only while the panel is on screen, up to 60 a
  second, as sharp as a Retina screen (two device pixels to a CSS pixel).
  While the page moves (the user scrolling, or three changes within a
  quarter second, as in a scroll or an animation) they are drawn at one
  pixel per CSS pixel instead, which reaches the panel in about 15ms,
  until the page has been still for 150ms. A single change, a click, a
  key or a blinking caret, is drawn sharp: text MUST NOT flip between
  sharp and blurred. A hidden panel costs nothing. ⌘C and ⌘X put the page's
  selection on the Mac's clipboard (a headless Chrome copies to one of its
  own); ⌘L, ⌘R, ⌘[ and ⌘] are the address bar, reload, back and forward.
  The user may open any page; BRW-3 is about what agents may read, and the
  panel says when agents cannot use the page it shows.
- **BRW-10** What an agent last did in the tab MUST show under it, as a
  person would say it ("Clicked “Save”"), and where it clicked is marked
  for a moment. An element is named by its label, never its value, which
  for a password field is the password.
- **BRW-11** An agent MAY ask for a site with `browser_request_site`,
  saying what for. Only the user's Allow adds it: the request shows over
  the task's browser, which opens for it, and in the message center, with
  a banner while the window is away (when "Agents waiting on you" is on).
  The tool waits up to two minutes and says what the user chose, or that
  no answer came yet; the same request again waits on the one already
  asked. No `confirm` an agent passes can stand in for the user's answer.
- **BRW-12** "Take over" (offered while an agent is using the browser,
  BRW-15) MUST keep the task's agents out of its tabs until "Hand back":
  every browser tool is refused while the user holds it, reads included, so nothing an agent does or reads overlaps the
  user signing in. The tool says why, so the agent can tell the user what
  it was about to do. Holding outlives the tab: a browser started again is
  still held.
- **BRW-13** An alert, confirm or prompt the page opens MUST be shown over
  the panel and answered there (OK, Cancel, a prompt's text), or by an
  agent with `browser_dialog`. A headless Chrome draws none, and the page's
  scripts wait on it: an agent's action that opened one comes back at once
  saying so, rather than waiting on a page that cannot answer, and nothing
  else is done until it is answered.
- **BRW-14** A window a page opens (a link with `target=_blank`,
  `window.open`, a sign-in popup) MUST become a tab of the task, next to
  the tab that opened it, and the active one. When it closes, by the
  page closing itself as sign-in windows do or by the user or an agent
  closing it, the tab that opened it is active again if it is still open.
- **BRW-15** While an agent is using the browser, the panel MUST say so,
  and the user MUST be kept out of the whole panel: an overlay over the
  page names the agent and what it last did, tab changes included
  ("Switched to “Admin”"), and takes the user's clicks, keys and wheel;
  the tab strip (switch, new, close), the address bar and back, forward
  and reload do nothing. Only "Take over" lets the user in, and "Hand
  back" gives the browser back. An agent is using the tab while one of
  its browser calls runs, and between calls for as long as its pane is
  still working on its turn, up to a minute after its last call: an agent
  thinks between calls, and an overlay that came and went with each call
  said nothing. With no agent using it, the page is simply the user's.
- **BRW-16** A task MUST have one or more tabs, one of them active,
  shared by the user and the task's agents: everything acts on the active
  one (the panel, the agents' tools, input, dialogs). When an agent
  switches tab, the panel follows. Agents list, open, switch and close
  tabs with `browser_tabs`, `browser_new_tab`, `browser_switch_tab` and
  `browser_close_tab`, and every outline lists the tabs. Closing the last
  tab leaves an empty one: a task's browser never has none.
- **BRW-17** A tab MUST be named by its page's title, and by its site
  while it has none (loading, or a page without one); "New tab" when it
  has neither. Nobody names a tab.
- **BRW-18** The user MAY save sign-ins for agents to use: a site, a
  username and a password, in Settings → Browser, or from the panel's
  "Save sign-in" after typing one into the page (BRW-20). The password
  goes to the keychain, never `config.json`; the site and username are
  in `config.json`. A site is a host with or without a port: `localhost`
  alone is every port of this machine's (one app runs on :4200 for one
  task and :4210 for another), and a site elsewhere without a port is
  its default port only. A site's subdomains are other sites.
- **BRW-19** An agent MUST NOT see a saved password. `browser_sign_in`
  has the app fill the username, the password, or both (a login asks
  for them on one page or on two) into the fields the agent names, and
  press Enter if asked; it answers with the page, never the password. A
  password is filled only on a page of the site it was saved for, only
  into a password field (an agent could otherwise put it in a text box
  and read it back), and never over plain `http` to a site that is not
  this machine. While a filled-in password is on the page, until the
  page navigates, `browser_evaluate` and `browser_screenshot` are
  refused (either could read it back) and the outline hides that
  field's value even if the page shows it.
- **BRW-20** "Save sign-in" in the panel MUST save what the user typed
  into the page's sign-in form: the password is read from the page's
  password field by the app itself and goes straight to the keychain,
  never through the window. The username is the form's, and is asked for
  when the form has none (a login that asked for it on the step before).
- **BRW-21** An agent MAY copy a sign-in from another of the task's tabs
  into the active one with `browser_copy_session`: the other page's local
  storage, session storage and cookies are written into the active
  page's, which is then reloaded. It is for an app signed in on one port
  of this machine (the one its sign-in server lets through, CORS) and
  needed on another. Both pages MUST be on this machine, so nothing is
  copied off it or from a site agents only browse. The agent is told
  what was copied by name and count, never a value. An item already
  there under the same key is replaced; others are kept. Cookies are
  copied only between two hosts (`localhost` and `127.0.0.1`): one
  host's ports share them already. Refused while a filled-in password is
  on either page (BRW-19).
- **BRW-6** The tabs MUST come back where they were: each task's tabs'
  pages and which was active are kept in `config.json`, and opened when
  the task's browser is next needed. A config from before tabs had one
  page, which comes back as one tab. Deleting or finishing a task closes
  its tabs.
- **BRW-7** The browser MUST stop with the app: asked to close, then its
  process group killed after 3 seconds, alongside the agents' own grace
  period. A browser that dies is started again when next needed.
- **BRW-8** Nothing the browser does MUST wait on the main thread or the
  async runtime. Chrome is started on the blocking pool and spoken to over
  a pipe (`--remote-debugging-pipe`) by two threads of its own. A pipe, not
  a port: with a debugging port open, any process on the Mac could drive
  the browser and every site signed in to there.

Code: `browser/` (`chrome.rs` finding and running it, `cdp.rs` the
protocol, `page.rs` a tab's actions, `snapshot.rs` the outline, `sites.rs`
BRW-3, `tools.rs` the tools, `panel.rs` and `input.rs` the panel's side,
`requests.rs` BRW-11, `control.rs` BRW-12, BRW-13 and BRW-15, `tabs.rs`
BRW-16 and BRW-17, `events.rs` Chrome's events and BRW-14, `sign_in.rs`
BRW-18 to BRW-20, `session.rs` BRW-21),
`commands/browser.rs`, `mcp.rs` (`dispatch`), `BrowserPanel.tsx`,
`BrowserSettings.tsx`, `lib/browserInput.ts`, `Terminals.tsx`.

Known gaps:
- Agents cannot see the page's frames from another site (an embedded
  sign-in or payment form): the outline is the main page's.
- Passkeys, password managers and extensions do not work in the app's own
  profile.
- `browser_copy_session` does not copy IndexedDB, where some sign-ins are
  kept (Firebase's).
- A `<select>` list's options are not drawn when it opens in the panel:
  choose with the arrow keys and Enter, or by typing an option's first
  letters. Agents use `browser_select`.
- The mouse pointer does not change over links and text fields.

## 19. Specs

A task can have a spec: what the work must do, how it will be done, and
the steps to get there, agreed before an agent starts and checked when it
is done. Each repository in the task has a spec of its own, a folder of
three files committed on the task's branch, so it goes through review
with the code and stays in the repository after the task, the branch and
the app are gone. The Spec tab, first of a task's tabs, drafts each file,
holds it while you edit it, and commits it when you approve it.

A spec kept beside the task (`SPEC.md` in the task folder) was deleted
with the task, and nothing ever checked the work against it: an agent was
told what done meant, and nobody asked afterwards whether it got there.

The shape follows Kiro's specs: requirements, then design, then tasks,
each reviewed before the next is drafted.

### What a spec is

- **SPEC-1** A repository's spec MUST be the folder `specs/<task>/` in its
  worktree, `<task>` being the task's branch with `/` as `-`
  (`specs/ACME-123-fix-login-race/`). The `specs/` folder is set per
  repository in the Repos view. It holds:
  - `requirements.md`: Goal, Requirements, Out of scope, Open questions.
    Each requirement is a list item with its id first (`R-1`, `R-2`, …),
    written as WHEN a condition THE SYSTEM SHALL a behaviour, so that
    someone can check it. One without an id is numbered by its place.
  - `bugfix.md`, in place of `requirements.md` for a bug: Current
    behaviour, Expected behaviour, Unchanged behaviour (WHEN … THE SYSTEM
    SHALL CONTINUE TO …), each item with its id, and Open questions.
    Feature or bug is chosen when the spec is started.
  - `design.md`: how this repository will meet them. Approach, what
    changes where, how it meets the other repositories (an endpoint's
    shape, an event's fields), error handling, how it will be tested.
  - `tasks.md`: numbered steps (`- [ ] 3. Add the limiter (R-1, R-2)`),
    each naming the requirements it serves, ticked when done.
- **SPEC-2** A task in several repositories MUST have a spec in each, for
  that repository's share of the work. A requirement that two
  repositories meet together is in both, and each design says the side of
  it that is its own. Another repository's requirement is referred to by
  its folder name: `web R-2`.
- **SPEC-3** A repository whose spec setting is off MUST NOT get spec
  files in its worktree. Its spec is kept in the app's folder instead
  (`<config folder>/specs/<repository>/<task>/`, same files), so it
  outlives the task on this machine, though not in git. The tab says
  which.

### Drafting and approving

- **SPEC-4** The files MUST be drafted in order and each approved before
  the next is drafted: requirements (or bugfix), then design, then tasks.
  Quick spec drafts all three at once, and they are approved together.
- **SPEC-5** Each draft MUST be a fresh one-shot run (Claude Code,
  Sonnet, no MCP, nothing that writes or runs), streamed into the editor:
  - Requirements: one run over the ticket (`TICKET.md`, fenced off as data
    and not instructions) and the task's repositories, which writes every
    repository's requirements, so the split is decided in one place. It
    invents no requirement the ticket does not imply; what the ticket
    leaves unclear goes under Open questions. A task with no saved ticket
    is drafted from its name.
  - Design: one run per repository, allowed to read its worktree, over its
    approved requirements and the other repositories'.
  - Tasks: one run per repository over its approved requirements and
    design.

  A redraft over text in the editor asks first; a failed draft puts back
  what the editor held.
- **SPEC-6** Approving MUST write the file into the worktree and commit it
  on the task branch, as the user, with only the spec's own files in the
  commit: whatever an agent has staged or changed there stays as it was.
  The message says which file of which task ("Approve the design for
  ACME-123."). A draft or an edit not approved reaches nobody, and the
  tab says so. Drafts are kept in the task folder (`.spec-drafts/`) until
  approved, so neither switching tasks nor quitting the app loses one.
- **SPEC-7** Changing an approved file MUST mark the files after it out of
  date. Sync redrafts them from it: tasks already ticked are kept, steps
  for new requirements are added, and a step whose requirements are all
  gone is marked for the user to remove, never removed by itself.
- **SPEC-8** The tab MUST show, per repository, each file's state (not
  started, draft, approved, out of date), the requirements, and the
  tasks ticked of the total. A requirements file with no requirements is
  warned about: they are what done means.
- **SPEC-9** A task whose worktree already has `specs/<task>/` MUST open
  it: a task cut from a branch that has one, or a second task for the
  same ticket on the same branch.

### Working to it

- **SPEC-10** The context file (PANE-13) MUST name each repository's spec
  folder and carry its requirements above the ticket, as the user's own:
  they are what done means, they win where they and the ticket disagree,
  what they put out of scope is left alone, and open questions are asked
  rather than guessed. Past 8 KB they are cut, and point at the files.
- **SPEC-11** An opening prompt for a task with a spec (`task_prompt`: the
  launch dialog, Start agent, handoffs) MUST end by pointing at the spec
  folders by full path, so an agent started inside one repository finds
  the others too.
- **SPEC-12** Start agent on the tab MUST start an agent on the approved
  tasks: at the task folder for every repository, or in one repository
  for its own. Its prompt tells it to work through `tasks.md` in order and
  mark each step done as it finishes it.
- **SPEC-13** An agent MUST mark a step done with the `spec_task` tool,
  which ticks it in `tasks.md` and updates the tab. Ticks are committed
  with the next approval or before Open pull requests (PR-1), with only
  the spec's files in the commit. A tick an agent makes by editing the
  file itself counts the same: the file is the record.
- **SPEC-14** Approving a change while agents in the task are running
  MUST offer to tell them: each is sent one line naming the files that
  changed, typed into a terminal or sent as a prompt over ACP. Never on
  its own, since a line typed into a busy agent interrupts it.

### Checking against it

- **SPEC-15** Check against spec MUST run, per repository, a one-shot run
  allowed to read the worktree, over its approved requirements and the
  branch's changes from its branch point (`git::baseline`). It answers
  per requirement: met, not met or unclear, with the files and lines that
  show it. The answer is shown beside each requirement, with the commit
  it was checked at, and marked stale once the branch moves on. It is
  kept in the task folder, never committed.
- **SPEC-16** A drafted pull request description (PR-4) MUST, for a
  repository with a spec, link its spec folder and list its requirements
  with the latest check's answer, or say none was run.

### From Start work

- **SPEC-18** Start work, with an agent picked, MUST offer "Write a spec
  first", on until turned off, and remembered. With it the task is made
  and the ticket moved as without it, but no agent starts: the task opens
  on its Spec tab with the requirements being drafted (unless the task
  already has a spec, SPEC-9), and Start agent there starts the agent
  Start work had picked (SPEC-12).

### Moving from `SPEC.md`

- **SPEC-17** A task with a `SPEC.md` from before MUST offer it as the
  requirements draft of each of its repositories, and remove it once
  those are approved.

Code: `spec.rs` (the files, and what is read from them),
`commands/spec.rs` (where a spec lives, approving, ticking),
`commands/spec_draft.rs` (drafting, checking), `commands/task_context.rs`,
`git/only.rs` (committing only the spec), `mcp.rs` (`spec_task`),
`commands/open_prs.rs`, `SpecView.tsx`, `spec/SpecSide.tsx`,
`lib/specs.ts`, `tickets/StartWorkDialog.tsx`, `ReposView.tsx` (the
per-repository setting).

Known gaps:
- Drafting runs Claude Code, whichever agent will do the work.
- A spec is approved by whoever runs the app. There is no second
  approver, and the commit is the only record of who approved it.
- Kiro runs independent tasks side by side; here one agent works through
  them in order.
- A draft cannot be stopped once started.
- A step an agent ticks is an uncommitted change in the worktree until the
  next approval or Open pull requests, and shows in the Diff tab until then.
- Drafting and checking need Claude Code on the PATH.

## 20. Agents over ACP

An agent can run as a conversation instead of in a terminal. The app
starts the CLI as an [Agent Client Protocol](https://agentclientprotocol.com)
server, speaks JSON-RPC to it over its stdin and stdout, and draws what it
says: replies as Markdown, each tool call with its output or diff, its
plan, and its permission questions as buttons. It is chosen when starting
an agent ("As a conversation (ACP)" in the launch dialog, "· conversation
(ACP)" in the Chat view's menu), beside the terminal, which stays the
default. Otherwise the pane is an ordinary one: it counts toward PANE-2,
is stopped as PANE-5 says, comes back as PANE-7 says, and every list,
count and banner treats it like any other.

| Agent | ACP command | Picks a conversation up by | Checked against |
|---|---|---|---|
| `claude` | `claude-agent-acp`, installed separately (`@agentclientprotocol/claude-agent-acp`, formerly `@zed-industries/`) | load, resume, list | adapter 0.87.0 (and 0.22.2) |
| `copilot` | `copilot --acp` | load, list | 1.0.93 |
| `opencode` | `opencode acp` | load, resume, list | 1.18.35 |
| `gemini` | `gemini --acp` | load | 0.63.0, `initialize` only: it would not open a conversation for a personal Google account |

- **ACP-1** ACP MUST be offered only for an agent whose catalogue entry
  has an ACP command, and only when that command is on the PATH. The
  launch dialog remembers the last choice.
- **ACP-2** An ACP pane's state MUST come from the protocol, never from
  its output: a `session/prompt` in flight is working, an unanswered
  `session/request_permission` is asking, the prompt's answer is done
  (idle when it was cancelled), and an error ends the turn as done, with
  the error shown. Started or picked up and given nothing yet, it is idle.
- **ACP-3** The app's MCP server MUST reach the agent in `session/new`
  (and load, resume) as an `http` server, with the bearer token and
  `X-Villain-Pane` as headers: over the agent's stdin, never on a command
  line or in a file (MCP-2). Nothing is written for the CLI to load: no
  hooks, no plugin, no settings. An agent that takes no MCP server over
  HTTP starts without the app's tools, and the pane says so.
- **ACP-4** Text MUST reach an ACP agent whole, however long: the opening
  prompt, a handoff, review comments, conflicts, a line from the phone.
  PANE-11's limit and its files are for typing into a terminal. A prompt
  sent while a turn runs waits for it to end, and the conversation shows
  it waiting.
- **ACP-5** A permission question MUST show the agent's own options as
  buttons, and be answered only by the user. The app never answers one
  itself, and never chooses "always".
- **ACP-6** Esc or Stop MUST cancel the running turn (`session/cancel`),
  answer every open question "cancelled", as the protocol requires, and
  drop the prompts that were waiting, marked as not sent. The pane is idle
  once the agent says the turn was cancelled. Closing a pane stops the
  process as PANE-5 says.
- **ACP-7** The pane MUST keep the conversation (its last 2,000 entries),
  and the window MUST be told of a change (`acp:update`) only while the
  pane is on screen; one coming on screen asks for what changed since it
  last looked. A plain-text copy goes into the pane's scrollback, so a
  handoff's briefing (PANE-9), `pane_output` (MCP-4) and the phone
  (PHONE-6) read an ACP pane as they read a terminal. Thinking is left out
  of the copy.
- **ACP-8** An ACP pane MUST be called by the title its agent gives the
  conversation (`session_info_update`), cut as PANE-12 cuts one.
- **ACP-9** An ACP agent MUST pick a conversation up by its id. Restart
  (PANE-14) and a restore (PANE-7) use the id the agent gave, kept with the
  saved pane: `session/load`, which replays it into the pane, or else
  `session/resume`. Resume with no id takes the newest the agent lists for
  the folder (`session/list`). One that cannot be picked up is started
  new, and the pane says why. So two agents in one folder never pick up
  the same conversation, and Restart works for every ACP agent. A replay
  shows what the user wrote, without a CLI's record of its own local
  commands: `claude-agent-acp` runs `/model` itself as each session
  starts, and a picked-up conversation opened with that, its output and
  the user's first message run together as one.
- **ACP-10** On the phone, an ACP pane's keys MUST mean: Esc and Ctrl-C
  stop the turn; 1 to 9 choose the oldest open question's options in
  order, as the text copy numbers them. A line typed is a prompt (ACP-4).
- **ACP-11** An agent that will not open a conversation MUST say why in
  the pane, with how it says to sign in (its `authMethods`). The app never
  signs in for it.
- **ACP-12** The settings the agent offers for the session
  (`configOptions`: its mode, its model) MUST be shown and changed from the
  pane, and only by the user.
- **ACP-13** The app MUST offer the agent none of the protocol's client
  methods: the agent reads, writes and runs commands itself, as it does in
  a terminal, and an `fs/*` or `terminal/*` request is refused, not left
  waiting.
- **ACP-14** A turn that fails on a usage limit MUST raise the
  usage-limit notice (STATE-6), read from the error's message; it clears
  when the agent next works.
- **ACP-15** A conversation's text MUST have a size of its own (Settings
  → Conversation text, 9–24px), as a terminal's has, apart from the
  interface scale: the conversation and the message box follow it, every
  part in proportion, and open panes change at once. The bar under the
  message box (mode, model, usage) is the app's chrome and does not.

Code: `acp/` (`conn.rs` the protocol, `user.rs`, `conversation.rs`,
`entry.rs`, `read.rs`, `process.rs`), `pty/acp.rs`,
`commands/acp_panes.rs`, `agents.rs` (`AcpCommand`), `AcpPane.tsx`,
`lib/acpUpdates.ts`, `lib/types-acp.ts`. A real agent is checked with
`VILLAIN_ACP_AGENT="opencode acp" cargo test --lib real_agent -- --ignored`.

Known gaps:
- Claude Code runs over ACP only through the `claude-agent-acp`
  adapter, which bundles its own Claude Agent SDK: its models and features
  are that version's, not those of the `claude` on the PATH. An old one
  goes wrong in ways the app cannot see: 0.22.2, the last under
  `@zed-industries/`, has the model call its own tools without their
  schemas (`Read` with `path`, refused) until it looks them up. A slash
  command the user ran there is left out of a replay, with the adapter's
  own (ACP-9).
- Copilot's ACP mode is in public preview. Gemini CLI was checked only as
  far as `initialize`.
- Codex (`codex-acp`), Cursor, Goose and the rest of the ACP registry are
  not in the catalogue: each needs checking against a real install first,
  as PANE-1 asks.
- Only the launch dialog and the Chat view offer ACP. Start work, Start
  agent on a spec, the Diff tab's "send to an agent" and a handoff start a
  terminal.
- Drafting PR and ticket descriptions still runs `claude -p` (§3).
- The slash commands an agent offers are only suggested as `/` is typed.

## 21. Loops

An agent can be put on a loop: each time it ends a turn, the app runs the
checks of the repositories it works in (each repository's own command:
`bun run check`, `cargo test`), and when they fail, sends it what failed
and lets it go on. It goes round until they pass, or until it has used its
rounds, and then stops and tells you. Without one, you are the loop: you
read that the agent is done, run the tests, paste the failures back, and
again.

A repository's check command is set in the Repos view, or in the dialog
that starts a loop. The loop button in a task's pane bar starts one on the
agent pane in front; the bar under the pane bar shows how it is going.

- **LOOP-1** A repository MAY have a check command: what says the work in
  it is done. It runs in a checkout's worktree through `/bin/sh -c`, in the
  login shell's environment (PANE-4) with `NO_COLOR` set, standard error
  folded into standard output and nothing on standard input, and passes
  when it exits 0. It is set only by the user: never from a ticket, a spec
  or an agent, since it is run as the user, unattended.
- **LOOP-2** A loop MUST be started by the user on one running agent pane,
  with the most rounds it may take (5 unless changed, at most 20). It
  checks the repositories the pane works in: its own, for a pane started in
  one, else every repository of the task. Those without a check command
  are skipped, and with none at all it does not start. One loop per pane.
  An agent not in a turn when its loop starts is checked at once;
  otherwise at the end of the turn. For an agent over ACP the dialog
  offers its mode (ACP-12) as it stands, changed only if the user changes
  it: one that asks before every command holds its loop at each, as
  Claude Code in its default mode did at an `ls`.
- **LOOP-3** A loop MUST act when its agent ends a turn, as the CLI
  reports it (a Claude Code Stop hook, an ACP prompt answered: STATE-1),
  never on a timer or on output going quiet. A turn interrupted (STATE-2)
  is not an end: someone is at the keys.
- **LOOP-4** At a turn's end the loop MUST first see whether anything
  changed in the worktrees it checks (the commit, the diff, the untracked
  files) since its last failed check ended: measured from what the check
  left, so a check that writes files of its own (a coverage report, a new
  snapshot) is not taken for the agent's work. When nothing did, the agent most
  likely stopped to ask something, or gave up: the loop runs nothing,
  sends nothing, and waits on you, saying so. It goes on at the next turn
  that changes something. A turn that ends on a usage limit (STATE-6)
  waits the same way. Run again on nothing new, the same failures went
  back to an agent that had already said what it needed.
- **LOOP-5** Checks MUST run one repository after another, at most two
  loops' at a time in the app (the others queue, and say so), each command
  for at most 20 minutes. A command still running then is stopped as an
  agent is (PANE-5: SIGTERM to its process group, then SIGKILL), and the
  loop stops, saying why. So does a command that exits 127, the shell's
  code for one it cannot find, which is seldom the agent's to fix.
  Stopping a loop stops its check, and quitting the app stops every
  check as it stops the agents: in a process group of its own, a build
  would otherwise run on after the app had gone. Nothing
  that waits runs on the main thread or the async runtime. Every check is
  a real process, often a whole build: the cap keeps a dozen loops from
  building at once on one laptop.
- **LOOP-6** Checks that fail, with rounds left, MUST go back to the
  agent: for each repository that failed, its command, its exit code and
  the end of its output (150 lines, 12 KB, without colour codes), with
  the round it is and how many there are, and to end the turn once they
  pass. To a terminal as a file in the task folder (`CHECKS.md`) with a
  pointer typed (PANE-11), to an agent over ACP whole (ACP-4). Only
  between turns: when someone started a turn while the checks ran, their
  answer is thrown away and the loop checks again at that turn's end.
- **LOOP-7** A loop MUST end when its checks pass (passed), when they
  still fail after its last round (gave up), when the user stops it, when
  its agent exits or is closed, or when a check cannot run (LOOP-5).
  Ending stops nothing else: the agent stays as it is.
- **LOOP-8** While a loop runs, a finished turn MUST NOT read as your
  turn: the pane reads as working wherever its state shows (its dot, the
  "needs you" count, the dock, banners, Keep awake: STATE-4, STATE-7),
  since the app is still at work on it. Waiting on you (LOOP-4) or ended,
  it reads as done again, as of that moment, so the dock and a banner say
  so even if you looked at the pane during the checks. The banner and the
  message center say how the loop ended ("passed its checks", "still
  fails its checks after 5 rounds", why it stopped) in place of "has
  finished".
- **LOOP-9** The pane MUST show its loop under the pane bar: the round,
  what it is doing (waiting for the agent, queued, checking which
  repository), how it ended, and each check run, newest first, with every
  repository's result and output. Stop while it runs; Start again once it
  has ended. The window is told of a change (`loop:changed`), never polls.
- **LOOP-10** A task's context file (PANE-13) MUST name each repository's
  check command, and tell agents to run it before they end a turn, so an
  agent checks its work by the measure the loop will, loop or not. Setting
  a command rewrites the context files of the tasks that repository is in.
- **LOOP-11** A loop MUST come back with its agent when the app reopens
  (PANE-7). It is kept with the saved pane, as it goes: its rounds, the
  rounds used, what the worktrees were when its checks last failed, and
  whether it waits on you. Once the pane is back, and its agent has said
  it is ready (or after a minute), the loop starts again on it: one that
  waited on you waits again; any other checks at once, since the turn it
  was waiting for ended with the app, and goes on from the rounds it had
  used. A pane that waits to be reopened brings its loop back when it is.
  Quitting the app is not the end of a loop. One that passed, gave up, was
  stopped, or whose agent exited on its own is not kept. Its check runs
  are not kept: the bar starts again with the next one. Before this, a
  restart ended every loop, and the agent came back with nothing driving it.

Code: `loops.rs` (the registry and the driver), `loops/check.rs` (running
a command), `git/snapshot.rs` (what changed), `pty.rs` (`turns`,
`set_looping`), `attention.rs`, `commands/loops.rs`,
`commands/task_context.rs`, `LoopBar.tsx`, `ReposView.tsx`.

Known gaps:
- Only the checks decide: the reviewer (DIFF-6) is not part of a loop
  yet, nor is a spec's check.
- A loop ends where you take over: it does not open a pull request, and
  does not follow CI or review comments after one (§8).
- A check is its command's exit code: a flaky test sends the agent after
  a failure that is not there.
- An agent asks for permissions on a loop as it does off one, and holds
  the loop until you answer: whether it asks is its own setting (ACP-12).
  Claude Code over ACP in its `auto` mode runs the commands a fix needs
  without asking; in `acceptEdits` it still asks before running one.
- Checked against Claude Code over ACP (`claude-agent-acp` 0.87.0, `auto`
  mode) and a fake ACP agent. Claude Code, Copilot, OpenCode and Gemini in
  a terminal report their turns' ends the same way (STATE-1), but have not
  been run on a loop.

---

## Template for a new area or feature

```markdown
## N. Name

What the user sees and can do, in a paragraph or a few bullets.

- **AREA-1** What MUST be true, in one or two sentences. Why, if it is not
  obvious — especially if it was learned from a bug.
- **AREA-2** …

Code: where it lives.

Known gaps:
- What it does not do yet, or does wrong, that we know about.
```
