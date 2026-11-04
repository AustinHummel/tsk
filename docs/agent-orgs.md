# tsk in a hierarchy of agents

How tsk can fit an org of agents: a coordinator that starts subagents,
which start subagents, all working at once, keeping their knowledge in a
vault of markdown notes with strict limits on how much one note may hold.
The mechanics are in the [README](../README.md); this is one way to use
them, worked out in one org's trial. Take what fits; where an org already
has a process that works, that process wins.

## One task per session

One arrangement that works: every agent session gets a task, and a
subagent gets a task that is a descendant of its parent's task. Not every
task up the chain has an agent
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
it holds, and it opens with what that author already has in progress, so
a session that is working with nothing started sees it at once: start
the task before the first tool call on it. `tsk plan --where agent` shows
who holds what.

A session reports results on the card without rewriting it: `tsk edit
<task> --append "found: ..."` adds a dated paragraph as one event. When
the shape of the plan turns out wrong, `tsk adopt <parent> <task>...
--narrative "..."` moves tasks under another parent with their ids,
history and requirements intact, so every link in the vault still
resolves; the number a task leaves behind stays vacant.

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

Usually nothing else in the plan is needed to work the task. When an
agent finds it needs more, that is often a missing requirement or a
missing doc link, worth adding rather than reading wider.

`--full` adds every ancestor's body. `--json` gives the same for a program.

For the person at the top, who reads the tree rather than one task,
`tsk ls` is one level at a time, each line carrying the id (`t16`, the
handle every page and note uses) and the leaves under it (`7 open, 2
ready, 3 done`); `tsk plan --depth N` cuts the outline at a level; and
`tsk plan <task>` on a task with no children says what else attaches to
it (the open tasks carrying its fields, the requirements on it), so a
quest root that holds its work by `quest=` rather than by nesting is
never a blank answer.

## Notes and the tree: a skeleton, not a filing system

The tree of tasks and the vault of notes have different shapes. A plan
follows how an engagement unfolds (understand, build, hand over); a vault
follows what is known (a product, a customer, a process). Mirroring one
into the other files knowledge where the work happened instead of where it
will be looked for, and tasks are far more numerous and shorter-lived than
the things worth knowing. So it seems better not to make the vault's
folders follow the tree, and instead to use the tree as the skeleton of
*intent* and link the two explicitly.

One way to sort what gets written, three kinds with one home each:

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

Sessions on one machine can share one store (`--store` at the main
checkout). Sessions that must carry their own copy, in a worktree or on
another workstation, let git bring the copies together: run `tsk
git-setup` once per machine, and a `git pull` or branch merge records the
other copy's HEAD as pending; the next tsk command replays both sides'
events since the fork in one chronological order, so every copy converges
on the same ids and the same plan, renumbering any colliding ids and
reporting the map. A field changed differently on both sides stops the
replay until someone chooses `--prefer ours` or `--prefer theirs`; a leaf
whose status moved on both sides is reported, the later event standing. For an
agent, the rule is: after a pull, run `tsk ready` (which settles any
pending merge) before writing, and treat a refused write as a conflict to
read with `tsk merge --dry-run` and resolve. Commit the store after the
merge like any other change.

## Speed as the org works

A command's cost does not grow with the history. The store keeps a
disposable cache (`.tasks/cache/`, never committed) of the plan each
snapshot loads to and the statuses each HEAD implies, keyed by the hash
they derive from; a command reads those two files, not every object and
every event. On a plan of 664 tasks with 966 events, `ready` and `plan`
answer in under 20 ms and a change takes under 50 ms. Only the views
that show history read it. Pruning is for size, not speed.

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

## Processes as types

An org has processes: a review checklist, a release procedure, a
session's close. An agent follows them from a skill or a command. There
is a symmetry with task types that seems worth using, with one guideline
that keeps it from turning the plan into a script: **a type holds the
scope a process owes, not its steps.** Each item is something that must be true
at the end, stated so that someone else could check it without knowing
how it was done ("the change is covered by a test that fails without
it"), not an instruction ("run the tests"). The how stays in the skill,
with its judgement and its exceptions; the type links the skill with
`--doc`, so an instance's `context` shows the process one hop away.

A process is a type when the org needs to *know*, per instance, that its
scope was covered, not just trust it: the instance's children are the
record, `plan` shows what is done, a dropped item is a recorded
exception with a reason, a `--force` is the signal that the type is
wrong. It is also a type when the process changes and running work must
be re-checked: editing a type lists every instance that now owes the
change, which no skill can do on its own. A process that is one
judgement call, or advice, is not a type; the task links the skill with
`doc` and that is enough.

Types tend to work best small, three to seven items. An item that applies
only sometimes can carry a `--when`, so the agent that drops it on an
instance it doesn't fit is following the type, not bending it. A change
to a type is in effect a change to an instruction, so whatever review the
org gives a skill probably belongs here too.

```
tsk type add change "A change to the product" --doc skills/change/SKILL.md
tsk type item change tested "Covered by a test that fails without it"
tsk type item change noted "User-facing note written" --when "the change is visible to users"
tsk apply change 1.4
```

## User-defined fields

Fields hold whatever a workflow needs that the model does not name:
`agent`, `run`, `priority` of a kind the org defines, `review=needed`, a
ticket number. They are strings, shown on the card and in context, carried
in `--json`, written into each rendered page's front matter, and usable as
`--where` filters; a field set on a task counts for everything under it.
Requirements take fields too (`set --req r3 kind=interface`).
