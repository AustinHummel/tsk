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
| `add <title> [--by "Who: why"] [-b body]` | A root task. Children only come from `breakdown` or `amend`. |
| `breakdown <task> [-F file]` | Children, narrative, and the disposition of every requirement, in one event. `--template` prints the file to fill in; with no `-F` it opens in `$EDITOR`. |
| `amend <parent> <title> --narrative "..."` | Add a child to a broken-down parent along with the scope it covers. |
| `edit <task> [-b body \| -F file] [--title] [--narrative]` | Change the card. |
| `link <pred> <succ> "<reason>" [--conditional]` | A dependency. `--conditional` makes the successor tentative. |
| `require <from> <on> "<reason>"` | A technical requirement: scope, not ordering. |
| `by <task> <person> "<reason>"` | Who requires it. |
| `move <req> [--from t] [--on t]`, `unlink <req>` | Refactor a requirement. |
| `set <task> key=value ... [--unset key]`, `set --req <req> ...` | User-defined fields on a task or a requirement. |
| `doc <task> [path] [--rm path]` | Link a task to a document it serves or produced; list with no path. |
| `type add`, `type item`, `type edit-item`, `type show`, `type list` | Reusable scope. |
| `apply <type> <task>` | The task becomes a parent of `core` plus one child per item. |
| `spike`, `confirm`, `start`, `pause`, `complete`, `uncomplete`, `drop` | State changes, each one event. `start` and `complete` take a group. |
| `show <task>` | The card with everything needed to work it. |
| `context <task> [--full]` | The chain of intent from the root to the task: who asked, each level's narrative, docs and fields, then the card, its requirements, siblings and children. |
| `plan [task] [-a] [--where k=v]` | The outline with status. |
| `ready [--blocked] [--where k=v]` | What can start now, and what each blocked leaf waits on. |
| `loops` | Every dependency loop and the requirements forming it. |
| `stale`, `forced`, `log [task]` | Review views. |
| `diff [task] [--from E] [--to E] [--git [REV]]` | What changed in the plan (or one task) between two events; by default since the store's last git commit. |
| `render [--dir d]` | One markdown page per task, plus an outline, for a vault to browse and link. |
| `backlinks <task>` | Markdown files in the project that link to the task's page. |
| `prune [--before E] [--dry-run]` | Drop old snapshots; every event and the log stay. |
| `config [key [value]]` | `author`, `editor`, `render.dir`, `lock.timeout`, `lock.stale`. |

Plumbing: `cat-object <hash>`, `hash-object [file]`, `ls-snapshot [event]`,
`rev-list`. Every view takes `--json`.

Tasks are addressed by id (`t7`), outline number (`1.2.4`), or exact
title when it names one task. Ids and numbers are fixed: nothing is ever
renumbered. Reasons and bodies with spaces are quoted, as with `git
commit -m`; they may begin with a dash.

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
(default 30) and then says who holds it; a lock older than `lock.stale`
seconds (default 60), which only a crashed process leaves, is broken and
reported. Both can be set per store with `tsk config` or per process with
`TSK_LOCK_TIMEOUT` / `TSK_LOCK_STALE`.

Each event carries an author: `TSK_AUTHOR` if set, else the store's
`author` config, else the OS user. A session that sets `TSK_AUTHOR` signs
its own events on a shared store.

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
`forced`, `author`), nothing else. A refusal exits 1 with one message on
stderr saying what and why. A typical agent loop:

```
tsk context 1.2 --json                # the chain of intent: read this first
tsk ready --json --where agent=me     # what can start, of what I'm assigned
tsk breakdown 1.2 --template          # the form
printf '...' | tsk --json breakdown 1.2 -F -
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

A store has one line of history: two checkouts that both add events
diverge, and there is no merge yet. Keep one store that every writer
reaches (`--store`), rather than a copy per branch.

`tsk prune --before <event>` removes the snapshots of older events and the
objects only they reach, so a long history stays small; every event and
every log line stay, and a pruned event's plan is reported as pruned when
asked for. Prune to the oldest event you still diff against.

## Model

- **Task**: id, title, parent, body (markdown, yours), narrative (set by
  breakdown), types, priority, due, docs, fields.
- **Requirement**: `{from, on, reason, blocking, conditional, fields}`, one
  record for a dependency (blocking), a technical requirement, and a
  "required by person" entry. Shown from both sides.
- **Type**: items with their own scope and requirements (`core`,
  `item:<id>`, or a task), optionally `--pending <task>`.
- **Event**: kind, parent event, snapshot, time, author, message, subjects,
  group, forced.

Derived: a parent is complete when its children are; a parent as
predecessor is complete when everything under it is; a child depending on
its parent is a loop like any other; ready = leaf, not tentative, no loop,
every blocking requirement (own or inherited) satisfied. A field set on a
task is inherited by everything under it for `--where`.

This repository plans itself: `tsk log` in a clone shows how 0.2 was
built, and [tasks/](tasks/README.md) is its rendered plan.
