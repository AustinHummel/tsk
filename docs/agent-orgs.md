# tsk in a hierarchy of agents

How tsk fits an org of agents: a coordinator that starts subagents, which
start subagents, all working at once, keeping their knowledge in a vault of
markdown notes with strict limits on how much one note may hold. The
mechanics are in the [README](../README.md); this is the way to use them.

## One task per session

Give every agent session a task, and give a subagent a task that is a
descendant of its parent's task. Not every task up the chain has an agent
running; that cannot be guaranteed when the tree is deeper than the agents
can nest. But every running agent has a task, and the tree holds the intent
at every level whether or not anyone is running there. That is the point:
the tree is the org's memory of *why*, independent of who is awake.

```
tsk set 1.4.1 agent=session-42 run=runs/2026-11-02-context-cmd
TSK_AUTHOR=session-42 tsk start 1.4.1
```

The field says who holds the task and where its working folder is; the
author stamps every event the session writes. `tsk ready --where
agent=session-42` is that session's queue, including anything under a task
it holds. `tsk plan --where agent` shows who holds what.

## The chain of intent

The first thing an assigned agent reads is `tsk context <task>`. It is the
smallest accurate scope for the work:

- the root's requester and reason: what all of this is for;
- each ancestor's narrative: why *this* task exists among its siblings,
  which is the piece of intent that a task's own card never states;
- the docs and fields at every level: the vault nodes and the assignments
  along the way;
- the task's card, what it requires of others and what others require of
  it (including what is required of its ancestors, which is who waits on
  this work), its blocks, siblings and children.

Nothing else in the plan is needed to work the task. An agent that needs
more has found a missing requirement or a missing doc link, and should add
it rather than read wider.

`--full` adds every ancestor's body. `--json` gives the same for a program.

## Notes and the tree: a skeleton, not a filing system

The tree of tasks and the vault of notes have different shapes. A plan
follows how an engagement unfolds (understand, build, hand over); a vault
follows what is known (a product, a customer, a process). Mirroring one
into the other files knowledge where the work happened instead of where it
will be looked for, and tasks are far more numerous and shorter-lived than
the things worth knowing. So: do not make the vault's folders follow the
tree. Use the tree as the skeleton of *intent* and link the two explicitly.

There are three kinds of writing, and each has one home.

1. **Intent and decisions.** Why the plan has the shape it has: the
   requester's reason, each narrative, each requirement's reason, each
   drop reason, each forced event. This lives in tsk itself, is tiny
   (about a kilobyte per task), and is kept forever.
2. **Knowledge.** What an agent at a *different* task, or a person a year
   on, would need. This lives in the vault, at the node where it would be
   looked for, under the vault's own size rules. The task links to it with
   `tsk doc`; the note links back to the task's rendered page
   (`[[t7]]`), so the history of how a node came to be is one hop away.
3. **Working state.** What is needed only to resume *this* task: partial
   findings, a checklist of what was tried, the handoff when a session is
   interrupted. This lives in the run folder named in the task's `run`
   field. It is useful for hours or days and then dead, and it is what
   fills disks. It dies with the task, under whatever pruning the org
   already does for run folders. When the task completes, whatever in it
   turned out to be knowledge is moved to the vault (kind 2) and whatever
   explains the plan goes into the task (kind 1); the rest is not kept.

The test for a sentence: *would an agent at another task need this?* Then
it is knowledge, and goes to the vault. *Does it explain why the plan is
shaped this way?* Then it is intent, and goes into tsk. Otherwise it is
working state, and it belongs in the run folder.

`tsk render` gives the vault a browsable copy of the tree (`tasks/`),
regenerated from the store, so a person in Obsidian can walk from a note
to the task that shaped it and up its chain, without the vault's structure
depending on the plan's.

## Working at once

Every change takes the store's lock for a few milliseconds; readers never
wait. Agents in other directories name the store (`--store`, or
`TSK_STORE` for the session) instead of changing directory. Each command
is one event, so there is no partial state for another agent to see. A
command refused with "locked by another tsk" after the timeout means a
process died holding the lock or something is very slow; the message says
who held it and how to clear it.

Keep one store that every writer reaches. Two copies of a store that both
gain events cannot be merged yet (a store has one line of history, like a
branch without merge). In an org that works in git worktrees, point every
session at the store in the main checkout rather than at its worktree's
copy, and commit the store from there.

## What the store costs, and pruning

An event is its own record (a few hundred bytes) plus the one path of the
tree it changed: a task object, and one small tree object per level of
depth above it. A status change (`start`, `complete`, `pause`) changes no
task, so it reuses the previous snapshot outright and costs the event
alone. For a plan of a few thousand tasks that is kilobytes per event, not
the hundreds of kilobytes a flat snapshot would be.

When a history is long, `tsk prune --before <event>` removes the
snapshots of events older than that one and every object only they reach.
Every event stays, so `log`, `show`'s history and `forced` are unchanged,
and `.tasks/log` is the complete record regardless. Only "what did the
plan look like then" is lost for the pruned events, and asking for it says
so. Prune to the oldest event anyone still diffs against: the last release,
the last review. `--dry-run` says what would go.

## Git

Commit `.tasks/` with the vault. The store's own `log` file makes every
commit that touched the plan readable in the commit diff: one line per
event, in order. `tsk diff` with no arguments shows what a session changed
since the last commit, events and plan both, which is what a session
reports at its close. Render before committing so the pages in `tasks/`
match the store the commit carries.

## User-defined fields

Fields hold whatever a workflow needs that the model does not name:
`agent`, `run`, `priority` of a kind the org defines, `review=needed`, a
ticket number. They are strings, shown on the card and in context, carried
in `--json`, written into each rendered page's front matter, and usable as
`--where` filters; a field set on a task counts for everything under it.
Requirements take fields too (`set --req r3 kind=interface`).
