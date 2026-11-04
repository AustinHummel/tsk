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
2021, one binary, pure-Rust dependencies, about 6,000 lines in thirteen
modules under `src/`, 38 end-to-end tests under `tests/`. Version 0.5.0
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

Green is **38 passed, 0 failed** as of 0.5.0 (8 in `tests/cli.rs`, 13 in
`tests/agent_org.rs`, 6 in `tests/merge.rs`, 11 in `tests/trial.rs`, 0 in
`src/`), about 10 s on a warm `target/`, a few minutes on a cold one. A change that adds a test
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
  for Windows. Code must not assume either. One difference bit in 0.5.0:
  MSVC's main thread has a 1 MB stack and clap's debug-build command tree
  outgrew it (every Windows CI test overflowed at start-up while the GNU
  build here was fine), so `main` runs the command on a thread with 16 MB
  (`STACK` in `main.rs`). A green local suite does not prove Windows CI.
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

The rules. Austin settled them 2026-11-03, answering the onboarding
agent's six proposals (P1 to P6): "3 thought 6 are good, but agents
should be able to push to main and tag releases, at least until the first
major release (1.0.0)". The first three were settled before he ruled.

- Commits are authored "Austin Hummel" (the repo's git identity) with the
  trailer `Co-Authored-By: <model name> <noreply@anthropic.com>` naming
  the agent model that wrote them.
- Never force-push, never rewrite `main`, never delete a tag.
- CI must be green on three platforms before anything is tagged (the
  `release` type's first item).
- **Until 1.0.0, agents may push `main` and may tag releases** (Austin,
  2026-11-03, above; it replaces the proposed "agents never push `main`"
  and "only Austin tags"). A branch and a pull request stay allowed,
  never required. CI runs on every push to `main`, so after pushing,
  check that run and fix or revert a red `main` at once: `gh run list
  --branch main --limit 1` where `gh` exists, or the API, which needs no
  login for this public repo (`gh` is not installed on the development
  machine, 2026-11-03):
  `curl -s "https://api.github.com/repos/AustinHummel/tsk/actions/runs?branch=main&per_page=1"`
  (read `status` and `conclusion`). At the first major release, 1.0.0,
  this rule lapses: from then on, who may push `main` or tag is Austin's
  to decide, and he has not yet said.
- **Versions.** Patch (`0.4.x`) for a fix that changes no command's
  interface and no on-disk form; minor (`0.x.0`) for a new or changed
  command, or any change to what the store writes. The version in
  `Cargo.toml` and the tag always agree (the release type enforces it).
  (P3.)

## Store format and upgrades

Since 0.5.0 `.tasks/config` carries `format = 1` (`store::FORMAT`):
`init` writes it, the first write by a newer binary raises it, and a
binary meeting a format above what it knows refuses every command with
one sentence naming the version needed. A store from before 0.5.0 has
no line, counts as format 0, and reads as before. The snapshot header
separately carries `version: 2` (the tree form; 1 was the flat form),
and `read_snapshot` detects both. A 0.4.2 binary knows nothing of the
line, so it still fails only when it hits what it cannot parse (the
`adopted` event kind); from 0.5.0 on the refusal is clear.

Raise `FORMAT` only when something on disk changes shape (a new event
kind, a new field a reader must honour), as its own task in the store,
and keep reading every older form. 1 added the `adopted` event and a
task's `former` positions.

Operating rule now (the org's pilot rule, 2026-11-03): **a store's
machines upgrade tsk together, at a quiet point**, with the store
committed before and `tsk ready` and `tsk diff` clean after. A change to
tsk that alters what the store writes is announced in the release notes
as such.

Settled by Austin, 2026-11-03 ("3 thought 6 are good", on the proposals
P4 to P6 as written here):

- **Backward reading is unconditional.** tsk reads every store form
  any released version wrote (constraint 7); a release never requires
  migrating a store by hand. (P4.)
- **A store-level format version.** The first change to any on-disk
  form after 0.4.2 adds a `format` line to `.tasks/config` (or a
  `.tasks/format` file), written by `init` and by the first write of a
  newer binary, and a binary that meets a format newer than it knows
  refuses with one sentence naming the version needed, rather than
  misreading. Done in 0.5.0 (task 8.12 in the store), as above. (P5.)
- **The org fixes what the pilot finds.** A bug found by the org's
  use of tsk is a task here (with `--by` naming who hit it and how),
  fixed, tested, and released under "Commit, push and release". (P6.)
