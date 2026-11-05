# The planning philosophy behind tsk

Austin Hummel's way of tackling any problem, as observed while he planned a
small project by hand in October 2026 and then had this tool built to
match. The [README](README.md) has the rules and the commands; this is the
*why*, the mindset the rules come from. Written from what he did and said;
his words are quoted, the rest is the observer's reading.

## The one idea

**A plan is a tree of scopes, and every edge between scopes carries a
reason.** Everything else follows from taking those two things seriously.

Austin: "I follow the exact same process no matter what it is that I'm
trying to get done, and it has always worked beautifully well, as long as I
can get whatever to-do list I'm using to behave the way I need it to."

## Scope is the unit, not the task

A task is a scope with a name. Its children are not "things to do under
it"; they *are* it, cut into pieces that touch but don't overlap. That's
why a parent must say how its children fit together: the narrative is the
proof that the cut is complete. It's why breakdown is atomic ("we should
never be stuck in a state where a task has subtasks that only cover a part
of its scope"), why a child can't exist before it's defined ("the
identifier 1.3 should not be assigned without a definition of what it's
identifying"), and why nothing is ever renumbered: a position in the tree
is a position in the scope.

The payoff is that any task can be worked from a small, predictable
context: the task, the requirements other tasks put on it, its ancestors,
and sometimes the siblings along the way. Nothing else is needed. That is
what makes the method work at any depth: you never need the whole plan in
your head, only the path down to where you are.

The test for whether something is a child of a task is not "is it
related" or "was it found while working here" but: *is the parent
incomplete until this is done?* If the parent would be honestly complete
without it, it belongs elsewhere, and the narrowest task that does need
it is its parent. A child added after the breakdown says which part of
the parent it takes, so the parent's narrative stays the description of
the cut rather than a log of arrivals. When a task that had no children
gains one, the person cuts it in two and names both pieces; a tool that
wrote a placeholder would be doing the breakdown's job badly (Austin, on
seeing one: "That's hardly a step-by-step breakdown").

## Scopes that don't end

Some scopes have no last piece. Austin: "long-running tasks like 'Run
this business' ... will act more as a folder where scope is expected to
be added regularly ... even when all children are complete, it's not
complete itself, until it's manually closed." Such an **ongoing** task
is still a scope, and everything under it is still a piece of it (the
same test applies), but no finite set of children defines its
completion. Its narrative is therefore an admission test, what belongs
under it and what does not, rather than the proof of a cut; it is never
ready, never started or completed itself, and it is complete only when
someone declares the scope shut and nothing under it is open. Declaring
is intent, like a priority; closing is history, like a completion. A
childless ongoing task is a standing duty, waiting in the tree until a
piece of it becomes concrete. Nothing can honestly depend on an ongoing
task, since it never completes on its own: a dependency names the piece
that is waited on, or the finite task that means "it exists". A duty that
repeats is an ongoing task holding one next occurrence at a time, dated;
whoever completes one adds the next, and the tool says when that moment
has come. A date, in this picture, is a predecessor that completes by
itself: a task gated by one is blocked exactly as it would be by an
unfinished predecessor, and nothing fires, because status is history and
history is written only by people.

## Reasons are what make it recursive

A dependency isn't "A before B". It's "B requires *this* of A". The reason
is the thing. Austin: "a link reason is, itself, an AC/requirement on the
predecessor, and it's subject to be refactored or applied to a different
task as we breakdown and rework project plan."

When B is broken down, each reason goes to the one child that actually
needs it. Every other child drops the dependency and becomes workable.
Repeat, and the blocked region shrinks to exactly the pieces the reasons
touch. Austin, on why: "I can start breaking down work and surgically
tracing out the pieces that actually have dependencies while Dana is
unavailable. Using this approach I can continue to work until only the
tiniest pieces are left, awaiting the conversation with Dana."

This is the core move of the method: **the unknown is contained, not
waited on.** A stakeholder's absence costs you the smallest possible slice
of the work. The same mechanism carries a technical requirement ("1.2
requires X from 2.3 because Y") and a person's requirement ("Dana needs
this because the same two picks always win"); they are one kind of record.

## Loops are information

Write down every dependency you can see, even when it makes a cycle.
"Loops are allowed (and even expected) in the dependency graph up until
work is being started." A loop isn't a mistake; it's the plan telling you
one of four things:

1. one of these tasks is too coarse: break it down and the loop falls
   apart into 1.1 → 2 → 1.2;
2. they share a prerequisite nobody has named yet: add the common
   predecessor;
3. one of the reasons is really an interface, an agreement on behaviour:
   make it a technical requirement instead of an ordering;
4. they're small enough to do as one: finish them together.

"Circular references tell you when additional breakdown needs done, or if
the tasks involved are already sufficiently small, they can be completed
simultaneously." A child depending on its parent is the same thing, not a
special case. The tool refuses to start or finish one task of a loop
alone; it wants the whole group, or a `--force` that it writes down,
because "if we ever have to use such a 'force' flag, that's probably a
good indicator that we're using the tool wrong, or the tool needs to be
updated to support a process flow that we previously didn't consider."

## Break down one layer at a time, and know when to stop

Define all of 1's children before touching 1.2's. The first layer follows
how the *engagement* unfolds (understand, build, hand over), not how the
product is built; the product split comes a layer later, once the
unknowns have been fenced off into their own task. "Each step of the
breakdown usually doesn't spawn into a large list of items."

Depth has diminishing returns: "any task could be broken down to a set of
lower-level instructions, but this is an act of diminishing returns beyond
a certain point." Stop when the next layer would be instructions. When a
layer *does* fan out into many siblings, they almost always share a shape,
and that shape is a type: scope written once and owed by every task that
carries it, so a change to the shape is a visible change to all of them.

A spike is the honest version of "I don't know how to cut this yet":
prove the thing, then break it down.

## History, never status

"I prefer timed event logs over statuses for data integrity and
extensibility." A task doesn't *have* a state; it has a history, and the
state is whatever the history implies today. That's why a parent is
complete when its children are, why uncompleting leaves the completion on
record, why a type edit can reopen an instance, and why a forced
transition is a fact rather than a flag someone cleared. It is also why
tsk is built like Git: immutable events over snapshots, derived views.

## Who asked, and why

Every root carries who required it and the reason. Not for blame: for
*pruning*. "The person and reason are useful when I'm deciding what tasks
are stale or have become irrelevant." A task whose reason no longer holds
is dropped with a reason of its own, and the record stays.

## What this asks of a tool

Only that it never fights the method: atomic breakdown with every
requirement handed down; one requirement record with a reason; loops
surfaced, not forbidden; a group completion for the small ones; state
derived from history; and a plain, non-interactive surface so a person or
an agent can run the loop (`ready`, `show`, `breakdown`, `loops`) without
the tool getting in the way. Everything Austin did by hand in markdown that
went wrong (titles copied wrong, one-sided links, a missing narrative) was
bookkeeping, never judgement. The tool takes the bookkeeping.
