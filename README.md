# tsk

A simple CLI for project planning and tracking. Command language inspired
by Git.

It breaks work down at any depth, and it is built the way Git is built:
every change is an immutable event pointing at a snapshot of the whole plan.
Status, readiness and dependency loops are computed from the history, never
stored. Breakdown is atomic. Loops are expected and surfaced. The tool
refuses rather than guesses, and records every `--force`.

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

Needs a Rust toolchain (`rustup`). From a clone:

```
cargo install --path .
```

Or straight from GitHub, which also updates an installed copy:

```
cargo install --git https://github.com/AustinHummel/tsk --force
```

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
| `type add`, `type item`, `type edit-item`, `type show`, `type list` | Reusable scope. |
| `apply <type> <task>` | The task becomes a parent of `core` plus one child per item. |
| `spike`, `confirm`, `start`, `pause`, `complete`, `uncomplete`, `drop` | State changes, each one event. `start` and `complete` take a group. |
| `show <task>` | The card with everything needed to work it. |
| `plan [task] [-a]` | The outline with status. |
| `ready [--blocked]` | What can start now, and what each blocked leaf waits on. |
| `loops` | Every dependency loop and the requirements forming it. |
| `stale`, `forced`, `log [task]`, `diff <task>` | Review views. |
| `config [key [value]]` | `author`, `editor`. |

Plumbing: `cat-object <hash>`, `hash-object [file]`, `ls-snapshot [event]`,
`rev-list`. Every view takes `--json`.

Tasks are addressed by id (`t7`), outline number (`1.2.4`), or exact
title when it names one task. Ids and numbers are fixed: nothing is ever
renumbered. Reasons and bodies with spaces are quoted, as with `git
commit -m`.

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
`forced`), nothing else. A refusal exits 1 with one message on stderr
saying what and why. A typical agent loop:

```
tsk ready --json                      # what can start
tsk show 1.2 --json                   # the card, with requirement ids
tsk breakdown 1.2 --template          # the form
printf '...' | tsk --json breakdown 1.2 -F -
tsk loops --json                      # anything to untangle
```

## The store

`.tasks/objects/<hash>` holds plain-text objects: `task`, `req`, `type`,
`snapshot`, `event`. `.tasks/HEAD` is the latest event. Commit the
directory with your project if you want its history in git too.

## Model

- **Task**: id, title, parent, body (markdown, yours), narrative (set by
  breakdown), types, priority, due.
- **Requirement**: `{from, on, reason, blocking, conditional}`, one record
  for a dependency (blocking), a technical requirement, and a "required by
  person" entry. Shown from both sides.
- **Type**: items with their own scope and requirements (`core`,
  `item:<id>`, or a task), optionally `--pending <task>`.
- **Event**: kind, parent event, snapshot, time, author, message, subjects,
  group, forced.

Derived: a parent is complete when its children are; a parent as
predecessor is complete when everything under it is; a child depending on
its parent is a loop like any other; ready = leaf, not tentative, no loop,
every blocking requirement (own or inherited) satisfied.
