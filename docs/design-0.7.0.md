# tsk 0.7.0: design note (writer keys and the shared-line merge)

Written 2026-11-19 by the release's design unit (root `t96` in this
repo's store), before any code. It settles the points the release plan
left to the builder. Line numbers are the tree at commit `d298a04`
(0.6.0). "Measured" means run in a scratch git repository with git
2.51.1 on Windows, with a logging shell script as the driver; everything
else was read. The plan and the decision behind it live in the private
org workspace (`org/org-development/multi-machine-parallelism/`); this
note is the public record of what the code will do.

## What the release is for

1. Two copies of a store mint the same id, and the merge renames one of
   them (`merge.rs:157-171`), so a holder of the old id writes to the
   wrong task.
2. A merge with the shared line moves outline numbers that were already
   published, because replay is chronological (`merge.rs:428-442`) and a
   position is a task's `seq` among its siblings (`plan.rs:121`).

Keys are opt-in per store and chosen per copy. tsk holds no letters, and
a store that never turns keys on behaves as 0.6.0 does, apart from its
format line.

## Settled

**The flag is `--upstream`:** `tsk merge <theirs> --upstream`. It is
allowed on any store, keyed or not.

**The key command.** `tsk key` prints this copy's key, or says it has
none, and says whether the store is keyed. `tsk key <letters>` sets it:
1 to 3 lowercase ASCII letters, anything else refused. It writes
`.tasks/key` (one line), adds `key` to the store's `.gitignore` through
`ensure_ignored` (`store.rs:405-412`) and to the `GITIGNORE` constant
(`store.rs:20`) for new stores. No event, no lock, like `git-setup`. No
`--reclaim`, no `rekey`. A store is turned keyed with the command that
exists: `tsk config ids keyed` (`main.rs:403`), which is committed with
the store. That write must also raise the format line, since a 0.6.0
binary that ignored `ids` would mint bare ids. On a keyed store a copy
with no key refuses id-making commands only, in one sentence that names
`tsk key <letters>`.

**Minting.** `Plan::new_task_id` and `new_req_id` (`plan.rs:94-103`)
have no store in reach, so the plan carries the key as a field that is
never serialised (the plan is cached through serde, `plan.rs:91`). The
counter is unchanged; the key is appended: `t150m`, `r98m`. The number
is read out of an id in three places, all in the merge
(`merge.rs:159`, `220`, `319`); each parses the whole tail and gets 0
for `t150m` today. `plan.rs:215` treats an all-digit reference as an
outline number and is unaffected.

**How replay recognises its own events (0.6.0 already stores it).** An
event has an optional `replayed_from`, the hash of the original it
stands for (`model.rs:288-290`). In `perform`:

- `identity` (`merge.rs:360-362`) is `replayed_from` if set, else the
  event's own hash, so a replay of a replay still names the first
  original (`merge.rs:479` writes the identity, not the hash).
- `have` (`merge.rs:376`) is every hash on our line plus every original
  our line stands for. Their head in `have` means already merged
  (`merge.rs:377-380`); each of their events whose identity or hash is in
  `have` is skipped (`merge.rs:393-397`).
- The fork point is the first of their events whose own hash is on our
  line (`merge.rs:386-390`).

The shared-line merge uses the same field in the other direction: it
builds the set from *their* line, and replays only those of our events
since the fork whose identity and hash are both absent from it. No new
field and no new on-disk form.

**The shared-line merge.** Their line is kept event for event, same
hashes. Starting from their head's snapshot, our remaining events are
replayed in our own order through the existing `apply`
(`merge.rs:301-342`), each written with `replayed_from` set to its
identity, then one `Merged` event. Rules that differ from the canonical
merge:

- A task of ours that is added always takes the next `seq` from the
  merged counter (as an adoption already does, `merge.rs:189-197`), so
  it lands after everything published. Today a non-colliding task keeps
  its own `seq` (`merge.rs:169-171`), which is failure 2.
- A keyed id is never renamed. A keyed id present on both sides with
  different content refuses the merge, nothing written, with the way out
  in the sentence. An unkeyed id of ours that is taken or passed is
  renamed as today (`merge.rs:164-168`); theirs is never renamed.
- Nothing of ours left to replay is a fast-forward to their head, even
  when we hold events since the fork (all of them are on their line in
  replayed form). Today's fast-forward test (`merge.rs:414`) is narrower.
- Their head already on our line by hash is "already merged". The
  `replayed_from` shortcut of `merge.rs:377` is not used in this mode:
  their line is the authority.
- Field conflicts stop it as now; `--prefer` as now.

**Status on both sides: a conflict** (replaced 2026-11-19, after the
blind review; the first design reported it and let replay order decide).
Status is derived by walking the chain oldest first (`state.rs:80-87`),
not by time, so after a shared-line merge our status event stood even
when it was older than theirs: an unpublished `start` replayed after a
published `complete` reopened the task for everyone. Austin, 2026-11-19:
"This sounds like standard merge conflict if I understand correctly. If
that's the case it should be up to the agent handling the merge to
figure out how it should be resolved on a case-by-case basis."

- *Status changed on a side* means a status event since the fork names
  the task: `start`, `pause`, `complete`, `uncomplete`, `drop`, `spike`,
  `close`, `reopen` (`state::set_by`, which `apply_event` folds). The
  status that side leaves it in is the one its last such event sets.
  A side that started and paused it has changed it, even back to where
  it was. The structural events that end a leaf's own status
  (`breakdown`, `amend`, `adopt`, `apply`) are not status changes; a
  close against scope gained on the other side is reported as before.
- Changed on both sides to different statuses is a conflict, handled as
  a field conflict is: nothing written, each task named with both
  sides' status, kind and time, the way out `--prefer`; through git the
  pending marker stays, a write refuses and a read shows our side until
  `tsk merge --prefer` settles it. The same status on both sides (both
  completed; `close` and `complete` both mean complete) is not.
- `--prefer` keeps every event and removes the task from the losing
  side's replayed status events (an event may name no task after that),
  so the preferred side's status stands and each original is still on
  the line by `replayed_from`.
- Tasks are compared by their id on the merged line (our subjects
  mapped), so an unkeyed id renamed by the merge is not taken for theirs.
- **The canonical merge follows the same rule.** It replaced "the later
  stands": the later event by the clock is the same silent override (a
  copy that starts a task it has not seen completed reopens it), and the
  rule is Austin's for a both-sides change, not for one mode. As with a
  field conflict there, a copy that merges the other's resolution may be
  asked again for the same task.

**The report.** Text, `--json`, and the `Merged` event's message all
carry it:

- `moved: t5m 3 -> 4` for each task on our pre-merge plan whose outline
  number differs on the merged plan (JSON `moved: [{id, from, to}]`).
- For each task whose status changed on both sides and was settled:
  `status on both sides: t7 ends complete (ours started 10:02, theirs
  completed 10:40; theirs preferred)`, or `; ours preferred`, or `; the
  same on both sides` (JSON `both_touched` gains `id`, `ends`, and
  `older_stands`: whether the event that stands is the older). One that
  is not settled is a conflict: `1.1 t7 Build: status (ours
  in-progress, started at 10:02; theirs complete, completed at 10:40)`.
- renames and resolutions, as 0.6.0.

**The pending marker.** Both early returns (`merge.rs:377-380`,
`410-412`) hand back our head without rewriting `HEAD`, so a
`merge <hash>` line whose target is already merged stays, and
`settle_pending` (`merge.rs:641-682`) re-runs and re-announces on every
command. The fix: when `perform` returns our own head and the HEAD file
named something to merge, the caller rewrites `HEAD` to the one line
(`set_head`, same value, under the lock).

**The git merge driver.** The driver (`merge.rs:689-705`) never opens a
store: it is handed two temp files and writes `ours` then `merge
<theirs>`. That stays. Which merge settles the marker is decided when
it is settled, where the store is open: on a keyed store, a merge that
came through git (the `merge` line, conflict markers, or `--git`)
settles as the shared-line merge; on an unkeyed store, and for a head
named by hand without `--upstream`, it is the canonical merge. The HEAD
file's form does not change.

**The driver refuses during a rebase, on a keyed store.** Measured:

- git runs the driver with the working tree's top as the current
  directory, also when git was started in a subdirectory, and `%P` is
  the path from there (`.tasks/HEAD`).
- During `git rebase` and `git pull --rebase` the sides are swapped (%A
  is the shared line), and `rebase-merge/` exists in the git directory;
  with `--apply` it is `rebase-apply/`. In a linked worktree the
  directory is under `.git/worktrees/<name>/`, and `GIT_DIR` is unset in
  a primary checkout, so the paths are asked for with `git rev-parse
  --git-path rebase-merge` and `--git-path rebase-apply`.
- During a plain `git merge`, a cherry-pick, a revert and a `stash pop`,
  no state file exists yet when the driver runs (no `MERGE_HEAD`, no
  `CHERRY_PICK_HEAD`), so those cannot be told apart.

So `git-setup` registers `tsk merge-driver %A %B %P`. The driver reads
`config` beside `%P`; with two arguments (a 0.6.0 registration) it looks
in `./.tasks`. If the store is keyed and either rebase directory exists,
it leaves `%A` untouched, exits 1 (git reports a conflict in
`.tasks/HEAD` and stops), and prints:

> tsk: this store is keyed and git is rebasing, which swaps the sides, so the shared line would be replayed instead of kept; run `git rebase --abort`, then `git merge <the shared line>`.

An unkeyed store's driver is unchanged in every case.

## Hard constraint 4, as it will read (draft; lands with the release)

> **Ids and positions are fixed.** A task is never renumbered or moved;
> children keep creation order. The exceptions are in `merge`, and only
> for work one copy holds alone. The shared-line merge (`--upstream`,
> and git's driver on a keyed store) keeps every id and position on the
> line it merges in; this copy's unpublished tasks take the next
> positions, and each number that moved is reported. A keyed id is never
> renamed by any merge. The canonical merge of an unkeyed store still
> renumbers colliding ids and reports the map.

## Where the plan and the code disagree

- **`replayed_from` exists.** The plan did not know whether 0.6.0 stores
  how a replay names its original. It does; nothing is added.
- **The driver cannot know the store is keyed as registered today.** It
  gets no path. The rebase refusal needs `%P`, so each machine re-runs
  `tsk git-setup` (or sets the driver value by hand) when it installs
  0.7.0.
- **`TSK_STORE` leaks into the test suite.** `--store` reads `TSK_STORE`
  (`main.rs:31`) and the tests' runner passes the environment through
  (`tests/common/mod.rs:25-34`). `init` uses the current directory
  (`commands.rs:222-224`), every later command uses the named store. So
  `cargo test` in a shell that has `TSK_STORE` set writes the tests'
  events into that store with the build under test. The org's agent
  shells have it set. Read, not run. The runner must remove
  `TSK_STORE`, `TSK_AUTHOR` and `TSK_NOW` before anything else in this
  release; until it does, the suite is run as
  `env -u TSK_STORE -u TSK_AUTHOR ...`. Filed as the first step of the
  format unit.
- **A prune before the fork already stops a merge, without a sentence.**
  Replay reads each event's parent snapshot (`merge.rs:462-465`); a
  pruned one fails there, before the claim, so nothing is written. The
  shared-line merge needs only our unpublished events' snapshots and
  their head's. The test decides whether the wording needs work.

## Limits this design leaves

- git gives the driver nothing to tell a merge from a cherry-pick,
  revert or `stash pop`, so the side is decided at settling (added after
  the blind review, 2026-11-19): the shared line is the store HEAD
  committed at config `shared` (default `origin/main`; a side branch's
  `@{upstream}` is not it). Theirs holds it: settled as recorded. Only
  ours holds it: settled with the sides swapped, and the report says so.
  Neither, or the ref can't be read: a write is refused, nothing
  written, naming `tsk config shared <ref>` and `tsk merge --upstream`
  (an explicit `--upstream` is the user saying theirs is the shared
  line, and is not checked); a read shows our side. When both hold it,
  everything past it is unpublished on both sides and either order
  moves nothing published.
- A machine with no driver gets conflict markers; settling them during a
  rebase is not refused.
- Our own unpublished `Merged` events are markers and are not replayed
  (`merge.rs:354-358`), so merging twice before publishing drops the
  first merge's report from history. Published ones are kept with their
  line.
- A merged chain's event times are no longer in order. Nothing in
  `state.rs` or `plan.rs` reads an event's time; the views print it.
- The driver leaves the marker in the commit git makes. The replay
  happens at the next tsk command and is uncommitted work until the
  session commits it. A copy that pushes before that hands the settling
  to whoever pulls.
- The shared line is only as current as the last `git fetch`: events
  pushed since are not known to be published.
