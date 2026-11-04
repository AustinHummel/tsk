# tsk architecture: the store, the model, and the modules

The detail behind the [architecture skill](../.claude/skills/tsk-architecture/SKILL.md):
what is on disk, how an event is made, how state is derived, and which
module does what. The constraints that void a change are in
[CLAUDE.md](../CLAUDE.md); the user-facing behaviour is the
[README](../README.md).

## The analogy, exactly

| Git | tsk |
|---|---|
| `.git/`, found by walking up | `.tasks/`, found the same way; `--store`, `TSK_STORE`, `-C` name one outright |
| Blob | A `task`, `req` or `type` object: immutable, SHA-256 of its bytes |
| Tree | A `tree` object: one parent's children in creation order, each with its task hash, the requirements sitting on it, and its own subtree hash |
| Commit | An `event`: kind, parent event, snapshot, time, author, message, subjects, group, forced, `replayed_from` |
| `HEAD` | `.tasks/HEAD`: the latest event's hash; a second line `merge <hash>` means another line of history is pending |
| `index.lock` | `.tasks/lock`: turn-taking for writers, token-stamped, breakable when its holder is dead |
| Ref update (compare-and-swap) | `.tasks/claims/<event>`: an atomic create of the parent being advanced from; a second writer on the same parent is refused (exit 3) |
| reflog | `.tasks/reflog`: every HEAD move on this copy; local, never committed; `tsk reset` returns to any |
| `git log` | `.tasks/log`: one line per event, appended by every commit; a 0.1 store got it backfilled |
| Shallow clone | `tsk prune --before <event>`: old snapshots go, every event stays |
| Rebase | `tsk merge`: both sides' events since the fork replayed in one canonical order |

## On disk

```
.tasks/
  HEAD            latest event hash; optional second line "merge <hash>"
  config          key = value: author, editor, render.dir, lock.timeout, lock.stale
  log             one line per event (time, hash, kind, message, subjects, author)
  objects/<hash>  "<type>\n<json>": task | req | type | tree | snapshot | event
  .gitignore      lock, *.tmp, reflog, cache/, claims/   (written by init)
  .gitattributes  * -text; log eol=lf merge=union; HEAD merge=tsk
  cache/          disposable memos keyed by hash (plan per snapshot, statuses per HEAD)
  claims/         one empty file per parent event ever advanced from
  reflog          local HEAD history
  lock            present only while a writer holds it
```

Objects are plain UTF-8, uncompressed, one JSON document after a type
line. `cat-object <hash>` prints one; `ls-snapshot [event]` lists a
snapshot's paths; `rev-list` walks the chain.

## The object model (`model.rs`)

- **Task**: `id` (`t<n>`, from a counter), `title`, `parent`, `seq`
  (creation order; the outline number is derived from it), `body`
  (markdown, the human's), `types`, `from_item` / `core_of` (set when a
  type is applied), `narrative` (set by breakdown), `priority`, `due`,
  `docs`, `fields` (a `BTreeMap<String, String>`), `former` (the
  positions it held before each adoption: `{parent, seq}`, each keeping
  its outline number vacant under the old parent).
- **Requirement**: one record for three uses. `from` is a `Requirer`
  (a task or a person), `on` the task it sits on, `reason` the text,
  `blocking` true for a dependency (`on` is the predecessor, `from` the
  successor), false for a technical requirement; `conditional` makes the
  successor tentative; `fields`.
- **TaskType** and **TypeItem**: reusable scope. An item has its own body,
  `reqs` (on `core`, `item:<id>`, or a task), optional `pending`, `when`,
  and `docs` that instances inherit.
- **Snapshot** (in memory) is flat: every task, req and type by id →
  hash, plus the two counters. On disk since 0.2 it is a `SnapshotHeader`
  (`version: 2`, root `tree`, `types`, counters) over `Tree` objects, so
  an event rewrites one path. `store::read_snapshot` loads the 0.1 flat
  form too (`version` absent).
- **Event**: `EventKind` (init, created, edited, broken-down, amended,
  adopted, linked, required, moved, unlinked, type-added, type-edited,
  applied, flagged-as-spike, confirmed, started, paused, completed,
  uncompleted, dropped, set, pruned, merged), `parent`, `snapshot`,
  `time` (UTC, to the second), `author`, `message`, `subjects`, `group`
  (a start or complete done together), `forced`, `replayed_from`.
- **Store format**: `.tasks/config` carries `format = 1` (since 0.5.0;
  `store::FORMAT`). `init` writes it, the first write by a newer binary
  raises it, `store_open` refuses a format above what the binary knows.
  A store without the line is format 0 and reads as before. The number
  moves only when something on disk changes shape; 1 added the `adopted`
  event kind and a task's `former` positions.

## How a write happens (`commands.rs` → `commit.rs` → `store.rs`)

1. `open_rw()`: settle any pending merge (refuse if it conflicts), take
   `.tasks/lock`, then read HEAD and load its snapshot into a `Plan`
   (through the cache when it has one) and the leaf statuses HEAD implies.
   The `Ctx` remembers the HEAD it loaded from.
2. The command checks and refuses first (`bail!` with what and why), then
   mutates the plan in memory. A new task gets the next id from the
   snapshot's counter; a new requirement likewise.
3. `Ctx::commit(Commit { kind, message, subjects, group, forced })` →
   `commit::write_on`: `plan.save` writes any changed task, req, type and
   tree objects and the snapshot header (unchanged subtrees keep their
   hashes, so a status change reuses the previous snapshot outright);
   the event object is written; `advance_head` claims the loaded parent
   (`claims/<parent>`, atomic create; already present means the store
   moved and nothing is written), sets HEAD, appends the reflog; the log
   line is appended; the status memo is carried forward.
4. The lock is dropped (removed only if it still carries this process's
   token).

A view (`open()`) does none of this: no lock, no settle-on-conflict beyond
a warning, HEAD and its snapshot read as immutable values.

## How state is derived (`state.rs`, `plan.rs`)

- **Leaf status** is a replay of the events (`apply_event`): started,
  paused, completed, uncompleted, dropped, flagged-as-spike, confirmed,
  each on its subjects; broken-down and applied end the parent's own
  status, and adopted and amended end the first subject's (a leaf
  promoted to a parent). Memoized per HEAD in the cache.
- **`Derived::compute`** builds, per plan state: a parent's status from
  its children (dropped children don't count; complete when every live
  child is; in progress when any is in progress, paused or complete;
  dropped when all are); the
  expanded dependency graph (a requirement on a parent applies to the
  whole subtree on both ends); loops (strongly connected components over
  blocking edges, a child depending on its parent included); blocks per
  task; readiness (leaf, not tentative, not in a loop, every blocking
  requirement own or inherited satisfied, not dropped); tentativeness
  (a conditional predecessor not yet complete).
- **Numbering** (`1.2.4`) is an index built once per plan state from the
  tree and `seq`, with each `former` position of an adopted task as a
  vacant slot under its old parent, so no task is ever renumbered; an
  adopted task's new `seq` is drawn from the task counter (so ids skip
  a number per adoption) and a merge replaying an adoption draws it from
  the merged line's counter. `Plan::resolve` accepts an id, a number, or
  an exact title that names one task, and names the task that vacated a
  number.
- **Fields** are inherited down the tree for `--where`: `Plan::field`
  walks ancestors.

Nothing here writes. Delete `.tasks/cache/` and all of it is recomputed.

## Merge, prune, render, in one paragraph each

**Merge** (`merge.rs`): two copies share every event up to the fork. The
replay takes both sides' events since then in one chronological order
(ties by the original's hash), diffs each against its own parent's
snapshot and applies it as a patch, producing a new event with
`replayed_from` set. Because the order is canonical, both copies converge
on identical events and ids; colliding ids are renumbered in that order
and reported; a field changed differently on both sides stops the merge
until `--prefer`. The git merge driver (`tsk git-setup` registers it)
resolves a `HEAD` conflict by writing `merge <theirs>` as HEAD's second
line, and the next command performs the merge. The merge event's message
names the pre-merge HEAD; `tsk reset` is the way back.

**Prune** (`prune.rs`): computes the closure of objects reachable from
every snapshot at or after the chosen event (and every line the reflog
can return to), and removes the rest. Events are never in the rest; a
pruned event's plan is reported as pruned when asked for.

**Render** (`render.rs`): one page per task from the current plan, with
front matter (`tsk`, `number`, `aliases`, `status`, fields), the path,
breakdown, requirements, docs and history; compares before writing so it
is idempotent. `backlinks` greps the project's markdown for `[[t7]]` or
`tasks/t7.md`.

## Where the design rationale lives

The decisions behind this shape are recorded outside this repo, in the
private org workspace that commissioned the tool (Austin Hummel's
austin-org, `org/org-development/process-study/`; not readable from a
clone of this repo, so a reader without access asks him, and this file
plus [PHILOSOPHY.md](../PHILOSOPHY.md) and [agent-orgs.md](agent-orgs.md)
are the public record):

- `cli-design.md`: Git's philosophy applied, version by version (0.1 to
  0.4.2), including what each release's dogfooding found.
- `task-model.md`: the task model as Austin stated it, the rules every
  refusal comes from.
- `cli-requirements.md` and `planning-philosophy.md`: what the tool must
  do and why; the public copy of the latter is [PHILOSOPHY.md](../PHILOSOPHY.md).
- `agent-org-fit.md`: the agent-org pass; the public copy is
  [agent-orgs.md](agent-orgs.md).

Those files are the why; this file is the what. When they disagree with
the code, the code is what ships, and the disagreement is worth a task.
