---
name: tsk-architecture
description: >-
  Design philosophy and placement guide for tsk, the task CLI built the way
  Git is built. Use before designing, placing, or reviewing any command,
  view, object or store change in this codebase; covers the load-bearing
  principles (immutable events over snapshots, derived state, one command
  one event, refuse don't guess), the module map at orientation altitude,
  where a new subcommand, view, object field or event kind goes, and what
  must stay out of which module. Pairs with CLAUDE.md (the constraints) and
  docs/architecture.md (the event and snapshot model in detail).
---

# tsk architecture

tsk is Git's object model applied to a plan: content-addressed immutable
objects, a chain of events (commits) each pointing at a snapshot (tree) of
the whole plan, `HEAD` naming the latest, and every view computed from
that. One idea explains most of the shape: **the store holds only what
happened; everything else is derived on read.**

## Load-bearing principles

- **Events over state.** A change is a new object and a new event; nothing
  is edited in place and no status is stored. Prevents: the silent
  flipped flag, the completion that leaves no record, the view that lies
  because a writer crashed halfway.
- **One porcelain command, one event, one commit path.** Every write goes
  `open_rw` (lock, then load) → change the plan in memory → `Ctx::commit`
  → `commit::write_on`, which saves the snapshot, writes the event on the
  loaded HEAD, claims that parent, moves HEAD, appends the log. Prevents:
  partial state another process can see, two commands' events interleaving,
  a write on a HEAD that moved.
- **Correctness does not depend on the lock.** The lock serializes
  writers for a few milliseconds; the claim on the parent event is what
  stops a lost write. Prevents: the 0.4.1 race (two writers after a stale
  lock was broken, one overwriting the other).
- **Refuse rather than guess.** A command that cannot be sure says what
  and why and exits 1; `--force` is the only override and is recorded.
  Prevents: a plan that quietly disagrees with the method it serves.
- **Ids and positions are fixed; numbers are derived.** `t17` is for
  life; `1.2.4` is computed from creation order. Prevents: references in
  notes and logs going stale.
- **Snapshots are trees, and the cache is keyed by hash.** An event costs
  one path of the plan; `.tasks/cache/` memoizes what a hash implies and
  so can never be stale. Prevents: cost growing with history, and the
  classic stale-cache bug.
- **Plain text, non-interactive, `--json` everywhere.** Objects are
  readable without the tool; every command works for a script or an agent.

## The map, at orientation altitude

Thirteen modules under `src/`; each has a `//!` header that is its own best
summary. Detail, including the object and event model, is
[docs/architecture.md](../../../docs/architecture.md).

| Layer | Module | Owns | Must never own |
|---|---|---|---|
| Objects | `model.rs` | The serde structs for task, requirement, type, snapshot header, tree, event; `EventKind`; `ObjectType`. | Logic; file I/O. |
| Store | `store.rs` | `.tasks/` discovery, hashing and object I/O, HEAD (ours, pending, conflicted), lock and claim, reflog, log file, cache, config. | Knowledge of what a plan means. |
| Plan | `plan.rs` | The in-memory plan loaded from a snapshot: tasks, reqs, types, the tree index, derived numbering, ref resolution (`t7`, `1.2.4`, exact title), field inheritance; `save` writes the tree. | Status. |
| Derived | `state.rs` | Leaf statuses replayed from events (`apply_event`), the expanded dependency graph, loops, blocks, readiness, tentativeness. | Anything written to disk. |
| Commit | `commit.rs` | `write_on`: snapshot → event → claim → HEAD → log line; the UTC clock that stamps events. | Deciding what to change. |
| Clock | `clock.rs` | The present as a `Moment` (from `TSK_NOW` or the system clock with the local offset, read once per command in `commands::load`), the stored forms of `after` and a dated `due` (a date; a local time with its UTC offset) and their comparison. | Deciding readiness (that is `state.rs`, which takes the `Moment`); anything a status depends on. |
| Porcelain | `commands.rs` | Every command that writes (add, breakdown, link, set, start …), the `Ctx` (store + plan + loaded HEAD + derived), `open`/`open_rw`, plumbing (`cat-object`, `hash-object`, `ls-snapshot`, `rev-list`), config, reflog, reset. | Rendering, merging. |
| Views | `views.rs` | Every read-only command: show, context, plan, ready, loops, stale, forced, log, diff. | Writes. |
| Breakdown file | `breakdown.rs` | Parsing and templating the `= child:` / `= narrative` / `= req` form. | The breakdown's semantics (those are in `commands::breakdown`). |
| Merge | `merge.rs` | Canonical replay of another copy's events, conflicts and `--prefer`, the git merge driver, `git-setup`, settling a pending HEAD. | Ordinary commits. |
| Prune | `prune.rs` | Removing old snapshots and the objects only they reach; never events. | — |
| Render | `render.rs` | `tasks/<id>.md` pages and the outline; backlinks. | Reading the store directly (it goes through `Ctx`). |
| CLI | `main.rs` | The clap `Cmd` and `TypeCmd` enums, global flags (`--store`, `-C`, `--json`), the dispatch `match`. | Behaviour. |

Hot spots by size: `commands.rs` (1,585 lines), `views.rs` (1,317),
`store.rs` (795), `merge.rs` (733). Adding to them is normal; splitting
one is a task of its own, not a side effect.

## Extension points

- **A new writing command.** Add the variant to `Cmd` in `main.rs` (clap
  derive; doc comment is the help text) and its arm in the dispatch
  `match`; a `pub fn` in `commands.rs` that opens with `open_rw()`,
  checks and refuses first (`bail!` with what and why), mutates `c.plan`,
  then `c.commit(Commit::new(kind, message, subjects))`, and prints the
  event through `commands::done` (text, or the event as JSON under
  `--json`). Which `EventKind`: a change to a typed field of the card
  (`due`, `priority`, `docs`) reuses `EventKind::Set` and copies
  `commands::doc`; a change to user fields copies `commands::set` (also
  `Set`); a change to the card's text (title, body, narrative) is
  `EventKind::Edited`, as in `commands::edit`; a new status transition is a new
  `EventKind` variant with its `name()` in `model.rs`, taught to
  `state::apply_event` (copy `commands::drop`, and the `transition`
  helper for anything that takes a group; `close`/`reopen` are the
  pattern for a transition that applies to one kind of task only); a
  change that adds or removes requirements copies `commands::link`
  (refuses, notes new loops); a change to a date copies `commands::when`
  (normalized through `clock::normalize`, a `Set` event). A command that
  adds scope under a task (`amend`, `adopt`) checks the parent with
  `parent_takes` and splits a leaf with `split_leaf`; it never writes a
  placeholder child. Then the README's command table (the release
  type's `readme` item).
- **A new view.** `Cmd` variant in `main.rs`; a `pub fn` in `views.rs`
  opening with `open()` (no lock), reading `c.plan` and `c.derived`, with
  a `--json` branch. Copy `views::ready` or `views::stale`.
- **A new field on an object.** Add it to the struct in `model.rs` with
  `#[serde(default, skip_serializing_if = ...)]` so every older object
  still loads (constraint 7 in `CLAUDE.md`); hash changes are expected,
  since a changed object is a new object. Show it in `views::show` and
  `render` if it matters to a reader.
- **A new on-disk form** (a new object type, a new file in `.tasks/`):
  `ObjectType` in `model.rs`, read/write in `store.rs`, the ignore list in
  `Store::init` if it is transient, and the upgrade policy in
  `CLAUDE.md` ("Store format and upgrades").
- **A new derived notion** (another kind of readiness, another block):
  `state.rs`, computed in `Derived::compute`; expose it through a view.
- **A test.** End-to-end in `tests/`, using `tests/common/mod.rs`'s `Proj`
  (a temp project with a store, run through the built binary). What a
  command does to one plan (a new command, a refusal, a field) goes in
  `cli.rs`; anything about many processes, `--store`, the log, cache,
  prune, render or old store forms in `agent_org.rs`; merging in
  `merge.rs`. When unsure, `cli.rs`.

## Boundaries and non-goals

- `store.rs` knows bytes, hashes and files, not tasks; `plan.rs` and
  `state.rs` know tasks, not files. Keep I/O out of the derived layer so
  views stay pure and cacheable.
- No command writes without `open_rw`; no view takes the lock.
- The rendered `tasks/` pages are an output, never an input: nothing reads
  them back except `backlinks`, which greps for links to them.
- Nothing in `state.rs` reads the clock: `Derived::compute` takes the
  `Moment` its caller passes, and a date touches readiness and blocks
  only, never a status, so the memoized statuses and a merge replay are
  the same on every machine and every day. `TSK_NOW` is the one override.
- No compression, packing or branches of a plan (left for later by
  design); no interactive prompts; no C dependencies. The local offset
  comes from the C runtime (Unix) and kernel32 (Windows) through
  declarations in `clock.rs`, not a crate, since both are linked by the
  standard library already.
- The agent-org guidance (`docs/agent-orgs.md`) is one org's way, not
  rules the tool enforces; don't encode an org's process into the binary.

## Verification

`~/.cargo/bin/cargo.exe test --locked` from the repo root, at low
priority; green is every test passing, none failing (the count, the
gotchas and the exact commands are in `CLAUDE.md`). An architectural
change is whole when: every existing test still passes unchanged (a test
that had to change is a behaviour change, and says so in the commit), the
new behaviour has a test that fails without it, and the rendered pages
produced by the changed binary for this repo's own store differ from the
committed `tasks/` only where the change meant them to.
