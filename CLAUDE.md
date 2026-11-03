# tsk: entry contract for agents

Read this before changing anything here. It says what the repo is, which
rules void a change, how to build and verify, how the repo tracks its own
work, and how changes reach `main` and a release. Depth is in the
[architecture skill](.claude/skills/tsk-architecture/SKILL.md) and
[docs/architecture.md](docs/architecture.md); the user-facing rules and
commands are the [README](README.md); the mindset is
[PHILOSOPHY.md](PHILOSOPHY.md).

## What it is

A CLI for planning and tracking work, built the way Git is built: every
change is an immutable event pointing at a snapshot of the whole plan, and
status, readiness and loops are computed from history, never stored. Rust
2021, one binary, pure-Rust dependencies, about 4,700 lines in twelve
modules under `src/`, 26 end-to-end tests under `tests/`. Version 0.4.2
(`Cargo.toml`). Public at https://github.com/AustinHummel/tsk, MIT.

It is Austin Hummel's tool, maintained by his org's agents (his decision,
2026-11-03). One live store depends on the installed binary
(`~/.cargo/bin/tsk.exe`): the org's own task store. Treat every change as
one that a running store will meet.

## Hard constraints

A change that breaks one of these is wrong whatever else it achieves.
Each is enforced by a test named in brackets where one exists.

1. **Objects are immutable.** Nothing under `.tasks/objects/` is edited,
   rewritten or deleted, except by `prune`, which removes only snapshots
   older than a chosen event and the objects only they reach. An object
   is named by the SHA-256 of its bytes, so `.gitattributes` forbids
   line-ending conversion on `.tasks/**`; never relax that.
   [`history_is_immutable_and_diffable`, `prune_keeps_every_event_and_the_current_plan`]
2. **One porcelain command writes exactly one event**, through
   `Ctx::commit` → `commit::write_on`, on top of the HEAD the plan was
   loaded from (never HEAD re-read at write time). Advancing HEAD claims
   that parent; a lost claim exits 3 with "the store moved ... nothing was
   written". Correctness never depends on the lock, which is turn-taking
   only. [`a_commit_on_a_parent_already_advanced_is_refused_not_overwritten`,
   `a_reported_save_is_always_on_the_line_even_with_stale_locks_broken_at_once`,
   `parallel_writers_never_lose_an_event`]
3. **Nothing derived is stored.** Status, readiness, blocks, loops,
   tentativeness and outline numbers are computed (`state.rs`, `plan.rs`).
   No stored status field, ever; `uncomplete` is a new event, not a
   cleared flag. The cache under `.tasks/cache/` is keyed by the hash it
   derives from and may be deleted at any time.
4. **Ids and positions are fixed.** A task is never renumbered or moved;
   children keep creation order. The one exception is `merge`, whose
   canonical replay renumbers colliding ids and reports the map.
   [`tasks_resolve_by_id_number_or_title`, `disjoint_changes_merge_with_ids_renumbered`]
5. **Breakdown is atomic**: children, narrative and the disposition of
   every requirement in one event, or nothing.
   [`breakdown_is_atomic_and_hands_requirements_down`]
6. **Never weaken a refusal.** A refused action (a partial breakdown, a
   spike on a parent, a start or complete inside a loop without the whole
   group, a write over a pending merge) stays refused, exits 1 with one
   sentence saying what and why, and the only override is `--force`,
   which is recorded on the event and listed by `tsk forced`. A new
   command that can be wrong refuses rather than guesses.
   [`loops_are_found_and_guarded`, `force_is_recorded`]
7. **Every store form ever written still loads.** A 0.1 flat snapshot
   loads next to a 0.2 tree snapshot (`store::read_snapshot` detects the
   form). A change to any on-disk form keeps reading every older form and
   is covered by a test with a fixture from the older version. See
   "Store format and upgrades" below.
   [`flat_snapshots_from_0_1_still_load`]
8. **Nothing needs a terminal.** Every view and change works with `--json`
   and without an editor; a command that would open one fails at once with
   the `-F` advice when there is no terminal.
   [`agents_get_a_non_interactive_path`]
9. **`tasks/` is generated.** `tsk render` is the only writer of the
   rendered pages; never hand-edit them.
   [`render_is_idempotent_and_backlinks_find_notes`]
10. **Pure-Rust dependencies only** (`Cargo.toml`): the development machine
    has no C toolchain, and the release builds static musl binaries.
11. **A behaviour change carries a test that fails without it**, in
    `tests/`, end-to-end against the binary. There are no unit tests in
    `src/` by design; don't start a parallel convention.

## Build, test, verify

The verification command, run from the repo root (`C:\Projects\tsk`;
agent shells reset their directory, so `cd` there in the same command or
pass `--manifest-path C:\Projects\tsk\Cargo.toml`), and what green means:

```
cd /c/Projects/tsk && nice -n 10 ~/.cargo/bin/cargo.exe test --locked
```

Green is **26 passed, 0 failed** as of 0.4.2 (8 in `tests/cli.rs`, 13 in
`tests/agent_org.rs`, 5 in `tests/merge.rs`, 0 in `src/`), about 10 s on
a warm `target/`, a few minutes on a cold one. A change that adds a test
updates this count in the same commit. `--locked` holds the committed
`Cargo.lock`; CI uses it too.

Gotchas that bite an agent:

- **`~/.cargo/bin/` is not on PATH in agent shells** on the development
  machine (found 2026-11-03). Call `~/.cargo/bin/cargo.exe` and
  `~/.cargo/bin/tsk.exe` by full path (`C:\Users\Austi\.cargo\bin\`), in
  Git Bash and PowerShell alike. A bare `cargo` or `tsk` that fails with
  "not found" is this, not a missing install.
- **Run the suite at low priority.** Two tests spawn 20-plus writer
  processes at once, and the machine belongs to a person who is using
  it. Git Bash: the `nice -n 10` above (mapped to BelowNormal). PowerShell:
  `(Get-Process -Id $PID).PriorityClass = 'BelowNormal'; & "$HOME\.cargo\bin\cargo.exe" test --locked`
  from `C:\Projects\tsk` (children inherit the shell's priority).
- **Toolchain:** locally the `x86_64-pc-windows-gnu` target (rustup +
  `rust-mingw`; there is no MSVC). CI runs `rustup default stable` on
  ubuntu, windows and macos; the release builds `x86_64-pc-windows-msvc`
  for Windows. Code must not assume either.
- **Tests write to the OS temp dir** (`tsk-test-<name>-<pid>`), never to
  this repo's `.tasks/`. The built binary under test is
  `CARGO_BIN_EXE_tsk`, so `cargo test` always tests the tree, not the
  installed copy.

The release build is `~/.cargo/bin/cargo.exe build --release --locked`
(`target/release/tsk.exe`, about 3.9 MB). To run a changed binary
against a scratch store: `target/debug/tsk.exe --store <tmp dir> init`.
Never point a dev build at a store anyone depends on: the org's
(`C:\Projects\austin-org\.tasks`) or this repo's own (`.tasks/` here);
both are written only by the installed binary.

Updating the installed binary (`cargo install --path . --force`) is an
upgrade of every store on the machine: see "Store format and upgrades".

## How tsk tracks its own work

This repository plans itself. Its store is `.tasks/` at the repo root,
committed; its rendered mirror is `tasks/` (`tasks/README.md` is the
outline). Each release is a root task with `--by` provenance; a release
task carries the `release` type, whose four items are the definition of
done (tests green on three platforms; `Cargo.toml` bumped and the tag on
it; five archives plus `SHA256SUMS` published and one verified; the README
naming every new or changed command, `--when` one was). `tsk type show
release` prints it.

For a change here:

- Set `TSK_AUTHOR` to a name that identifies your session before the first
  write: the model or role, the job, the date, for example
  `TSK_AUTHOR="opus agent-due-command 2026-11-04"`. The store's `author`
  config (`Claude (side session)`) is the original author's and mislabels
  anyone else's events.
- Use the installed binary for the bookkeeping (`~/.cargo/bin/tsk.exe`,
  or `--store C:/Projects/tsk` from elsewhere), not the build under test.
- A change starts as a task. A fix or feature on its own is a root:
  `tsk add "<title>" --by "<who>: <why>"`. Work that belongs to a
  release in progress is a child of that release's root, added with
  `tsk amend <root> "<title>" --narrative "<how it fits>"` (children
  come only from `breakdown` or `amend`); `tsk plan` shows the roots, and
  a release in progress is a root not yet marked `[x]` (its release-typed
  child still open). `start` it; `complete` it;
  a change that is not going to happen is `drop`ped with a reason.
- Before committing, `tsk render`, and commit `.tasks/` and `tasks/`
  with the change. `tsk log` in a clone is the record of how every
  version was built.
- Branches each carry their own copy of the store, so merging one can
  conflict on `.tasks/HEAD`. The merge driver from `tsk git-setup` (once
  per machine; registered on the development machine) records the other
  side as pending and the next tsk command replays it; without the
  driver, `tsk merge` reads the markers (README, "Merging copies of a
  store").

## Commit, push and release

What the history and workflows show (2026-11-03): every commit so far is
authored "Austin Hummel" and was pushed straight to `main` by the side
session that built the tool; CI (`.github/workflows/ci.yml`) builds and
tests on ubuntu, windows and macos on every push to `main` and every pull
request, posting failing output as a commit comment; a pushed `v*` tag
runs `.github/workflows/release.yml`, which tests, builds five platform
binaries and publishes a GitHub release with `SHA256SUMS`. Versions:
0.1.0 on 2026-10-30, 0.2.0 to 0.4.2 on 2026-11-02 and 11-03, each a root
task in the store.

Settled:

- Commits are authored "Austin Hummel" (the repo's git identity) with the
  trailer `Co-Authored-By: <model name> <noreply@anthropic.com>` naming
  the agent model that wrote them.
- Never force-push, never rewrite `main`, never delete a tag.
- CI must be green on three platforms before anything is tagged (the
  `release` type's first item).

**Proposed, awaiting Austin's OK** (written 2026-11-03 by the onboarding
agent; not his words). Until he rules, agents follow them as the stricter
reading:

- **P1. Agents never push `main`.** An agent works on a branch named for
  its job (`agent-<job>`), pushes only that branch, and opens a pull
  request against `main`, which is what runs CI on the three platforms
  for a branch (CI triggers on pushes to `main` and on pull requests,
  not on other branches). Austin merges. Direct pushes to `main` stay
  his.
- **P2. Only Austin tags a release.** A `v*` tag publishes public
  binaries; an agent prepares a release (version bump, README, the
  release task's items) on a branch and reports it ready, and he tags.
- **P3. Versions.** Patch (`0.4.x`) for a fix that changes no command's
  interface and no on-disk form; minor (`0.x.0`) for a new or changed
  command, or any change to what the store writes. The version in
  `Cargo.toml` and the tag always agree (the release type enforces it).

## Store format and upgrades

There is no store-level format version today (0.4.2): nothing in
`.tasks/` says which tsk versions can read it. The snapshot header
carries `version: 2` (the tree form; 1 was the flat form), and
`read_snapshot` detects both, but tasks, events, requirements and the
`log` and `HEAD` files carry no version at all. So an older binary
meeting a store written by a newer one fails only when it happens to hit
something it cannot parse, not with a clear message.

Operating rule now (the org's pilot rule, 2026-11-03): **a store's
machines upgrade tsk together, at a quiet point**, with the store
committed before and `tsk ready` and `tsk diff` clean after. A change to
tsk that alters what the store writes is announced in the release notes
as such.

**Proposed, awaiting Austin's OK:**

- **P4. Backward reading is unconditional.** tsk reads every store form
  any released version wrote (constraint 7); a release never requires
  migrating a store by hand.
- **P5. A store-level format version.** The first change to any on-disk
  form after 0.4.2 adds a `format` line to `.tasks/config` (or a
  `.tasks/format` file), written by `init` and by the first write of a
  newer binary, and a binary that meets a format newer than it knows
  refuses with one sentence naming the version needed, rather than
  misreading. This closes the gap above; it is its own task in the store
  when it happens, not a drive-by.
- **P6. The org fixes what the pilot finds.** A bug found by the org's
  use of tsk is a task here (with `--by` naming who hit it and how),
  fixed on a branch, tested, and released by P1 to P3.
