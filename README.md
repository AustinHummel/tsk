# tsk

A simple CLI for project planning and tracking. Command language inspired
by Git.

It breaks work down at any depth, and it is built the way Git is built:
every change is an immutable event pointing at a snapshot of the whole plan.
Status, readiness and dependency loops are computed from the history, never
stored. Breakdown is atomic. Loops are expected and surfaced. The tool
refuses rather than guesses, and records every `--force`.

Many people or agents can work one store at once, from any directory; the
store commits with your project and diffs like code; tasks link to markdown
and markdown links to tasks. How that fits a hierarchy of agents is in
[docs/agent-orgs.md](docs/agent-orgs.md).

## The process it serves

The mindset behind it is in [PHILOSOPHY.md](PHILOSOPHY.md); the short
version:

1. **One task for the problem**, in the requester's words, with who asked
   and why: `tsk add "Help Dana pick books" --by "Dana: the same two picks always win"`.
2. **Break it down one layer at a time, atomically.** Children cover the
   parent's scope exactly; the parent gets a narrative of how they fit;
   every requirement touching the parent is handed to the child it belongs
   to, or kept on the parent when all of them need it.
3. **Write every dependency you can see, with its reason**, even when it
   makes a loop. The reason is what the successor requires of the
   predecessor, and it travels down at the next breakdown.
4. **Look at `tsk ready` and `tsk loops`.** A loop means: break down one of
   its tasks, add a common predecessor, turn a reason into a technical
   requirement, or, if the tasks are small, finish them together. Each
   breakdown narrows what's blocked to the exact piece the reason touches,
   so work goes on while the stakeholder is away.
5. **Work the ready leaves.** Flag a spike when something must be proven
   before it can be broken down. Give a large set of siblings a type.
6. Repeat on whatever is still blocked or too big. Stop when the next layer
   would be instructions.

## Install

**From a release** (no Rust needed): download the archive for your platform
from [Releases](https://github.com/AustinHummel/tsk/releases), verify it
against `SHA256SUMS` from the same release, unpack, and put `tsk` (or
`tsk.exe`) on your PATH.

```
sha256sum -c SHA256SUMS --ignore-missing          # Linux, macOS, Git Bash
Get-FileHash tsk-*.zip | Select-Object Hash       # PowerShell; compare with SHA256SUMS
```

Every tagged version `vX.Y.Z` gets a release with a binary for Windows
x86-64, Linux x86-64 and arm64 (static, musl), and macOS arm64 and
x86-64.

**With Rust** (`rustup`), which also updates an installed copy:

```
cargo install --git https://github.com/AustinHummel/tsk --force
```

Or `cargo install --path .` from a clone. `tsk --version` says what you have.

## Commands

Process (porcelain):

| Command | What it does |
|---|---|
| `init` | Create `.tasks/` here. |
| `add <title> [--by "Who: why"] [-b body] [--priority p] [--due d]` | A root task. Children only come from `breakdown` or `amend`. Priority and due are free text, set only here. |
| `breakdown <task> [-F file]` | Children, narrative, and the disposition of every requirement, in one event. `--template` prints the file to fill in; with no `-F` it opens in `$EDITOR`. |
| `amend <parent> <title> --narrative "..."` | Add a child along with the scope it covers. A leaf parent is promoted first: its own scope becomes a `(core)` child. |
| `adopt <parent> <task>... --narrative "..."` | Move existing tasks under a parent, keeping their ids, history, children, requirements, docs and fields. A leaf parent is promoted as `amend` does; a broken-down one has the narrative appended. |
| `edit <task> [-b body \| -F file] [--append text] [--title] [--narrative]` | Change the card. `--append` adds a dated paragraph (`2026-11-04: ...`) and keeps what is there. |
| `link <pred> <succ> "<reason>" [--conditional]` | A dependency. `--conditional` makes the successor tentative. |
| `require <from> <on> "<reason>"` | A technical requirement: scope, not ordering. |
| `by <task> <person> "<reason>"` | Who requires it. |
| `move <req> [--from t] [--on t]`, `unlink <req>` | Refactor a requirement. |
| `set <task> key=value ... [--unset key]`, `set --req <req> ...` | User-defined fields on a task or a requirement. |
| `doc <task> [path] [--rm path]` | Link a task to a document it serves or produced; list with no path. |
| `type add [--doc]`, `type item [--when] [--doc] [--req] [--pending]`, `type edit-item`, `type show`, `type list` | Reusable scope; a type can stand for a process (see the agent-org guide). |
| `apply <type> <task>` | The task becomes a parent of `core` plus one child per item. |
| `spike`, `confirm`, `start`, `pause`, `complete`, `uncomplete`, `drop` | State changes, each one event. `start` and `complete` take a group. |
| `show <task>` | The card with everything needed to work it. |
| `context <task> [--full]` | The chain of intent from the root to the task: who asked, each level's narrative, docs and fields, then the card, its requirements, siblings and children. |
| `plan [task] [-a] [--where k=v] [--depth N]` | The outline with status: `[ ] 1.2 t3 Title`. A parent whose finished children are hidden says `(3 done)`; a childless task says what else attaches to it. |
| `ls [task] [-a] [--where k=v]` | One level, flat: the roots, or a task's children, each with the leaves under it: `(7 open, 2 ready, 3 done)`. |
| `ready [--blocked] [--where k=v]` | Your own tasks in progress first, then what can start now, and what each blocked leaf waits on. |
| `loops` | Every dependency loop and the requirements forming it. |
| `stale`, `forced`, `log [task]` | Review views. |
| `diff [task] [--from E] [--to E] [--git [REV]]` | What changed in the plan (or one task) between two events; by default since the store's last git commit. |
| `render [--dir d]` | One markdown page per task, plus an outline, for a vault to browse and link. |
| `backlinks <task>` | Markdown files in the project that link to the task's page. |
| `prune [--before E] [--dry-run]` | Drop old snapshots; every event and the log stay. |
| `reflog`, `reset <event>` | Every move of HEAD on this copy, and the way back to any of them. |
| `merge [theirs] [--prefer ours\|theirs] [--dry-run]` | Replay another copy's events onto ours. |
| `git-setup` | Register the git merge driver for `.tasks/HEAD` (once per machine). |
| `config [key [value]]` | `author`, `editor`, `render.dir`, `lock.timeout`, `lock.stale`, `color.ui`. |

Plumbing: `cat-object <hash>`, `hash-object [file]`, `ls-snapshot [event]`,
`rev-list`, `merge-driver`. Every view takes `--json`.

Tasks are addressed by id (`t7`), outline number (`1.2.4`), or exact
title when it names one task. Ids and numbers are fixed: nothing is ever
renumbered. An adopted task takes the next position under its new parent
and leaves its old number vacant (`tsk show 3` then says where `t3`
went), the way a dropped task keeps its line; rendered pages are named
by id, so links to them hold. Reasons and bodies with spaces are quoted,
as with `git commit -m`; they may begin with a dash.

The human views are coloured, subtly and in git's spirit, when stdout is
a terminal: `--color auto|always|never` (default `auto`), config
`color.ui` (`true`, `false`, `always`), and `NO_COLOR` are respected.
Output piped to a file or printed with `--json` is never coloured.

## Which store

By default tsk walks up from the current directory to the nearest
`.tasks/`, like Git. To work a store from anywhere, without changing
directory:

```
tsk --store /path/to/project add "..."     # the project, or its .tasks directly
TSK_STORE=/path/to/project tsk ready       # the same, for a whole session
tsk -C /path/to/project ready              # run as if started there (like git -C)
```

`--store` keeps relative paths such as `-F file` relative to where you are;
`-C` does not.

## Many writers at once

Every change takes `.tasks/lock` for the few milliseconds it needs to
write its event, so parallel commands from any number of processes
serialize cleanly and nothing is lost. Readers never lock. A command that
finds the lock held waits (with backoff) up to `lock.timeout` seconds
(default 30) and then says who holds it. A lock left by a process that
crashed is broken at once when tsk can tell its process has exited (same
machine), and otherwise once it is older than `lock.stale` seconds
(default 60); either way the break is reported. Both can be set per store
with `tsk config` or per process with `TSK_LOCK_TIMEOUT` /
`TSK_LOCK_STALE`.

The lock is for turn-taking; correctness does not depend on it. Before
HEAD moves from one event to the next, the writer claims the event it is
moving from with an atomic create of `.tasks/claims/<event>`; a second
writer that read the same HEAD finds the claim taken and stops with
"the store moved ... nothing was written. Run it again", exit code 3.
So two processes can never both commit on top of the same event, however
the lock was obtained, broken, or misconfigured: a reported save is
always on the line. Breaking a stale lock takes the file aside and checks
it is the one that was judged stale, by token, handing back any newer
lock it displaced; a displaced lock's owner sees its token gone and stops.

Each event carries an author: `TSK_AUTHOR` if set, else the store's
`author` config, else the OS user. A session that sets `TSK_AUTHOR` signs
its own events on a shared store, and `tsk ready` opens with the tasks
that author has in progress (`in progress (you):`), so a session that is
working with nothing started sees it.

## The breakdown file

```
= child: Decide on tech stack
Freeform scope for this child, until the next directive.

= child: Create repo

= narrative
How the children together fulfill the parent.

= req r3 -> 2
= req r4 -> all
```

## For scripts and agents

Nothing needs a terminal. `breakdown` and `edit` take `-F <file>` or `-F -`
(stdin); `breakdown <task> --template` prints the form to fill in, with
every requirement that must be dispositioned. When no `-F` is given and
there is no terminal, the command fails at once with that advice instead
of opening an editor. `--json` before any command makes views print JSON
and makes a change print its event (`event`, `kind`, `subjects`, `group`,
`forced`, `author`), nothing else, and never an escape code whatever
`--color` says. A refusal exits 1 with one message on stderr saying what
and why. `ready --json` carries `in_progress` (every leaf in progress,
with its `author` and whether it is `mine`); `plan --json` and `ls --json`
carry a parent's `counts` of the leaves under it. A typical agent loop:

```
tsk context 1.2 --json                # the chain of intent: read this first
tsk ready --json --where agent=me     # mine in progress, what can start
tsk breakdown 1.2 --template          # the form
printf '...' | tsk --json breakdown 1.2 -F -
tsk edit 1.2.1 --append "found: ..."  # a dated note on the card, nothing replaced
tsk loops --json                      # anything to untangle
```

## Tasks and markdown

`tsk doc <task> <path>` records the documents a task serves or produced;
`show` and `context` print them, so an agent reading its task finds the
notes at every level of its chain.

`tsk render` writes `tasks/<id>.md` for every task (folder from
`render.dir`), plus `tasks/README.md`, the outline. Each page carries front
matter with an alias of its number and title, so in Obsidian `[[t7]]`
resolves and shows as `1.2.4 Context command`; the page links its path,
children, requirements, docs and history. Pages are generated: do not edit
them, run `render` again. `tsk backlinks <task>` lists the markdown files
that link to a task's page, as `[[t7]]` or `tasks/t7.md`.

## The store, and git

`.tasks/objects/<hash>` holds plain-text objects: `task`, `req`, `type`,
`tree`, `snapshot`, `event`. `.tasks/HEAD` is the latest event. A snapshot
is a tree of per-parent objects, so an event costs its own record plus the
one path of the plan it changed; a status change costs the event alone.

`.tasks/cache/` holds memos of what the store implies (the plan a
snapshot loads to, the statuses a HEAD implies), keyed by the hash they
derive from, so they can never be stale. It is disposable and never
committed; delete it any time and the next command rebuilds it. With it,
a command reads two files instead of every object and every event, so a
store does not get slower as its history grows: on a plan of 664 tasks
and 966 events, `ready` and `plan` take under 20 ms and a change under
50 ms (Windows, i7). The views that show history (`log`, `show`,
`forced`, `diff`) still read it, and `prune` is for size, not speed.

`.tasks/config` declares the store's `format` (1 since 0.5.0; a store
from before carries no line and reads as before, and the first write by
a 0.5.0 binary adds it). A binary that meets a format newer than it
knows refuses with one sentence naming the version needed, rather than
misread the store; every older form is always read.

Commit `.tasks/` with your project. `.tasks/log` has one line per event
(time, event, kind, message, tasks, author), appended by every command, so
the diff of a commit that touched the store reads as a changelog; the lock
and temp files are ignored by a `.gitignore` the store carries. `tsk diff`
with no arguments shows what changed since the store's last git commit,
events and plan both; `tsk diff --git v1.0` since a tag. To keep the
rendered pages in step, render before committing, for example in
`.git/hooks/pre-commit`:

```
#!/bin/sh
tsk render >/dev/null && git add tasks
```

## Merging copies of a store

Two checkouts (a worktree, another workstation) that both add events
diverge. `tsk merge <their HEAD>` brings them back to one line by
replaying every event since the fork, from both sides, in one canonical
order: chronological, each side's own order kept, ties by the original
event's hash. Each event is diffed against its own parent and applied as
a patch, becoming a new event with the same kind, time, author and
message; `replayed_from` names the original, which is how a copy knows
what it already holds. Because the order is the same from either copy,
two copies that merge each other converge on the same events, the same
ids and the same snapshot, and have nothing left to exchange. Our own
events since the fork may get new hashes to take their chronological
place; `tsk diff` still finds the committed HEAD through `replayed_from`.
Ids that collide are renumbered in that order, on whichever side comes
later, and the map is reported (`ours t20 is now t21`; outline numbers
follow). A field changed differently on both sides is a conflict: nothing
is written, and `--prefer ours` or `--prefer theirs` decides. A leaf with
status events on both sides is reported; the later event stands.
`--dry-run` shows all of this without writing.

Git carries the objects (add-only, never a conflict); only `HEAD` can
conflict. Run `tsk git-setup` once per machine: it registers a merge
driver that resolves a `HEAD` conflict by recording the other side as
pending (`merge <hash>` on HEAD's second line), so `git pull` never
blocks, and the next tsk command performs the merge (a read warns and
shows our side if it conflicts; a write refuses until `tsk merge
--prefer` settles it). Without the driver, `tsk merge` with no arguments
reads the conflict markers git left in `HEAD`. The log merges as a union
and is rebuilt from history after a merge, or whenever it is found out of
step. After either path, commit `.tasks/` as usual.

**Any merge, or anything else, can be undone.** Nothing is ever deleted
by a merge; HEAD moves. Every move of HEAD is recorded in `.tasks/reflog`
(local to the copy, like git's), the merge event's message names the
pre-merge HEAD, and `tsk reset <event>` moves HEAD to any event whose
plan can still be read, rebuilding the log; `tsk reflog` lists the moves
with where each came from. `prune` keeps whole every line the reflog can
return to. So a merge you don't like is `tsk reset <the HEAD it names>`,
and the merged line is still there to come back to.

`tsk prune --before <event>` removes the snapshots of older events and the
objects only they reach, so a long history stays small; every event and
every log line stay, and a pruned event's plan is reported as pruned when
asked for. Prune to the oldest event you still diff against.

## Model

- **Task**: id, title, parent, body (markdown, yours), narrative (set by
  breakdown), types, priority, due, docs, fields, and the positions it
  held before it was adopted elsewhere (each keeps its number vacant).
- **Requirement**: `{from, on, reason, blocking, conditional, fields}`, one
  record for a dependency (blocking), a technical requirement, and a
  "required by person" entry. Shown from both sides; a person's is
  labelled `required by`, a task's `technical`, `predecessor` or
  `conditional`.
- **Type**: items with their own scope and requirements (`core`,
  `item:<id>`, or a task), optionally `--pending <task>` or `--when`;
  docs on the type and its items are inherited by every instance.
- **Event**: kind (`created`, `broken-down`, `amended`, `adopted`,
  `edited`, `started`, ... ), parent event, snapshot, time, author,
  message, subjects, group, forced.

Derived: a parent is complete when its children are; a parent as
predecessor is complete when everything under it is; a child depending on
its parent is a loop like any other; ready = leaf, not tentative, no loop,
every blocking requirement (own or inherited) satisfied. A field set on a
task is inherited by everything under it for `--where`.

This repository plans itself: `tsk log` in a clone shows how every
version from 0.2 on was built, and [tasks/](tasks/README.md) is its
rendered plan.

## Maintaining it

Start with [CLAUDE.md](CLAUDE.md): the constraints a change must keep,
how to build and verify, how the repo tracks its own work, and the
commit, push and release rules. The design is in
[docs/architecture.md](docs/architecture.md).
