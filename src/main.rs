mod breakdown;
mod clock;
mod color;
mod commands;
mod commit;
mod merge;
mod model;
mod plan;
mod prune;
mod render;
mod state;
mod store;
mod views;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// A simple CLI for project planning and tracking. Command language inspired
/// by Git: every change is an immutable event, every state is derived.
#[derive(Parser)]
#[command(name = "tsk", version, about, max_term_width = 100)]
struct Cli {
    /// Machine-readable output: views print JSON, and a change prints its event.
    #[arg(long, global = true)]
    json: bool,
    /// Run as if started in DIR (like `git -C`).
    #[arg(short = 'C', long = "chdir", global = true, value_name = "DIR")]
    chdir: Option<PathBuf>,
    /// The store to use: a `.tasks` directory or the project that holds one.
    /// Also read from `TSK_STORE`. Leaves the working directory alone.
    #[arg(long, global = true, env = "TSK_STORE", value_name = "PATH")]
    store: Option<PathBuf>,
    /// Colour the human views: `auto` (a terminal, unless `NO_COLOR` is set or
    /// config `color.ui` says otherwise), `always`, or `never`. Never for `--json`.
    #[arg(long, global = true, value_name = "WHEN", value_parser = ["auto", "always", "never"])]
    color: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a `.tasks/` store in the current directory.
    Init,
    /// Add a root task: the problem in the requester's words, with who asked and why.
    /// Children come only from breakdown, amend or adopt, and only when the task is not
    /// complete until they are.
    Add {
        title: String,
        /// Card body (markdown). `-` reads stdin.
        #[arg(short, long, allow_hyphen_values = true)]
        body: Option<String>,
        /// Who requires it, and why: `--by "Dana: the same two picks always win"`.
        #[arg(long = "by", allow_hyphen_values = true)]
        required_by: Vec<String>,
        #[arg(long)]
        priority: Option<String>,
        /// When it is due: free text, or a date (`YYYY-MM-DD`) or local time (`YYYY-MM-DDTHH:MM`)
        /// the views compare.
        #[arg(long)]
        due: Option<String>,
        /// Not workable before this date or local time; inherited by everything under it.
        #[arg(long, value_name = "DATE")]
        after: Option<String>,
        /// Declare it ongoing, with what belongs under it: its last child is not yet known,
        /// so it is complete only when closed.
        #[arg(long, allow_hyphen_values = true, value_name = "ADMITS")]
        ongoing: Option<String>,
    },
    /// Break a task down into children, atomically, from a breakdown file.
    Breakdown {
        task: String,
        /// Breakdown file (see `tsk breakdown --template`). `-` reads stdin.
        #[arg(short = 'F', long)]
        file: Option<String>,
        /// Print the editor template for this task and exit.
        #[arg(long)]
        template: bool,
    },
    /// Add a child to a task, with the part of the parent's scope it takes (--fit). A task
    /// is a child only when the parent is not complete until it is done; the narrowest such
    /// task is the parent. A parent with no children yet is cut in two: name the piece that
    /// holds its own work with --core.
    Amend {
        parent: String,
        title: String,
        #[arg(short, long, allow_hyphen_values = true)]
        body: Option<String>,
        /// Which part of the parent's scope this child takes, so a reader can tell it from
        /// its siblings. Required.
        #[arg(long, allow_hyphen_values = true)]
        fit: Option<String>,
        /// The parent's cut, rewritten: replaces its narrative (nothing is appended).
        #[arg(long, allow_hyphen_values = true)]
        narrative: Option<String>,
        /// The new child is not workable before this date or local time.
        #[arg(long, value_name = "DATE")]
        after: Option<String>,
        /// When the new child is due.
        #[arg(long)]
        due: Option<String>,
        /// When the parent has no children yet: the title of the piece that holds the
        /// parent's own work, beside the new one.
        #[arg(long, allow_hyphen_values = true, value_name = "TITLE")]
        core: Option<String>,
        /// The body of that piece, when the split changes it (default: the parent's body).
        #[arg(long, allow_hyphen_values = true, value_name = "BODY")]
        core_body: Option<String>,
    },
    /// Move existing tasks under a parent, keeping their ids, history, children and
    /// requirements, with the part of the parent's scope each takes (--fit).
    Adopt {
        parent: String,
        tasks: Vec<String>,
        /// Which part of the parent's scope the adopted tasks take. Required; recorded on
        /// each (`tsk edit <id> --fit` gives each its own).
        #[arg(long, allow_hyphen_values = true)]
        fit: Option<String>,
        /// The parent's cut, rewritten: replaces its narrative (nothing is appended).
        #[arg(long, allow_hyphen_values = true)]
        narrative: Option<String>,
        /// When the parent has no children yet: the title of the piece that holds the
        /// parent's own work, beside the adopted ones.
        #[arg(long, allow_hyphen_values = true, value_name = "TITLE")]
        core: Option<String>,
        /// The body of that piece, when the split changes it (default: the parent's body).
        #[arg(long, allow_hyphen_values = true, value_name = "BODY")]
        core_body: Option<String>,
    },
    /// Edit a task's body in $EDITOR, or set it from a file or argument.
    Edit {
        task: String,
        #[arg(short = 'F', long)]
        file: Option<String>,
        #[arg(short, long, allow_hyphen_values = true)]
        body: Option<String>,
        #[arg(long)]
        title: Option<String>,
        /// Replace the narrative (a parent's cut, or a childless ongoing task's admission test).
        #[arg(long, allow_hyphen_values = true)]
        narrative: Option<String>,
        /// Add a dated paragraph (`2026-11-04: ...`) to the body, keeping what is there.
        #[arg(long, allow_hyphen_values = true, value_name = "TEXT")]
        append: Option<String>,
        /// Which part of the parent's scope this task takes.
        #[arg(long, allow_hyphen_values = true)]
        fit: Option<String>,
    },
    /// Declare a task ongoing: its last child is not yet known, so it is complete only when
    /// closed; the text is what belongs under it. `--off` takes the declaration back.
    Ongoing {
        task: String,
        /// What belongs under it, and what does not.
        #[arg(allow_hyphen_values = true)]
        admits: Option<String>,
        /// The task is finite after all; its narrative stays.
        #[arg(long)]
        off: bool,
    },
    /// Close an ongoing task: declare that no more scope belongs under it. Refused while
    /// anything under it is open.
    Close {
        task: String,
        #[arg(allow_hyphen_values = true)]
        reason: String,
    },
    /// Reopen a closed ongoing task.
    Reopen { task: String },
    /// Dates on a task: not workable before --after (inherited by everything under it; also
    /// how a wait on someone outside the plan lapses at a deadline), and when it is --due.
    /// Nothing fires: tsk ready shows what a date is holding back and what is overdue.
    When {
        task: String,
        /// A date (`YYYY-MM-DD`, the start of that local day) or a local time
        /// (`YYYY-MM-DDTHH:MM`), stored with its UTC offset.
        #[arg(long, value_name = "DATE")]
        after: Option<String>,
        /// Free text; a date or local time is compared by the views.
        #[arg(long, allow_hyphen_values = true, value_name = "TEXT")]
        due: Option<String>,
        /// Remove a date: `after` or `due`. Repeatable.
        #[arg(long, value_name = "FIELD")]
        clear: Vec<String>,
        /// What the date stands for (`Northwest's written reply, or Tuesday morning
        /// without it`); recorded in the event.
        #[arg(allow_hyphen_values = true)]
        why: Option<String>,
    },
    /// Make PRED a predecessor of SUCC, for a reason. The reason is what SUCC requires of
    /// PRED. It is not a place to say "X is part of Y": that is a child (tsk amend), not a
    /// predecessor.
    Link {
        pred: String,
        succ: String,
        #[arg(allow_hyphen_values = true)]
        reason: String,
        /// SUCC's existence, not just its start, waits on PRED (a tentative task).
        #[arg(long)]
        conditional: bool,
    },
    /// Place a technical requirement FROM one task ON another. Not an ordering.
    Require {
        from: String,
        on: String,
        #[arg(allow_hyphen_values = true)]
        reason: String,
    },
    /// Record that a person requires a task, and why.
    By {
        task: String,
        person: String,
        #[arg(allow_hyphen_values = true)]
        reason: String,
    },
    /// Move a requirement to a different task at either end, keeping its history.
    Move {
        req: String,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        on: Option<String>,
    },
    /// Remove a requirement.
    Unlink { req: String },
    /// Set user-defined fields on a task (or, with --req, a requirement): `key=value`.
    Set {
        /// The task, or with `--req` the requirement id.
        target: String,
        /// `key=value` pairs. Keys are letters, digits, `_`, `-`, `.`.
        #[arg(allow_hyphen_values = true)]
        pairs: Vec<String>,
        /// Remove a field.
        #[arg(long, value_name = "KEY")]
        unset: Vec<String>,
        /// TARGET is a requirement id.
        #[arg(long)]
        req: bool,
    },
    /// Link a task to a document it serves or produced (a path or URL); list them with no PATH.
    Doc {
        task: String,
        path: Option<String>,
        /// Remove the link instead.
        #[arg(long, value_name = "PATH")]
        rm: Option<String>,
    },
    /// Task types: reusable scope stamped onto every task that carries one.
    #[command(subcommand)]
    Type(TypeCmd),
    /// Apply a type to a leaf: it becomes a parent of `core` plus one child per item.
    Apply { r#type: String, task: String },
    /// Flag a leaf as a spike: something must be proven before it can be broken down.
    Spike { task: String },
    /// Confirm a tentative task: its condition has been met.
    Confirm { task: String },
    /// Start work on one task, or on a group that forms a loop.
    Start {
        tasks: Vec<String>,
        #[arg(long)]
        force: bool,
    },
    /// Pause a task that is in progress.
    Pause { task: String },
    /// Complete one task, or a group that forms a loop.
    Complete {
        tasks: Vec<String>,
        #[arg(long)]
        force: bool,
    },
    /// Reopen a completed task.
    Uncomplete { task: String },
    /// Drop a task, with the reason.
    Drop {
        task: String,
        #[arg(allow_hyphen_values = true)]
        reason: String,
    },
    /// The card: everything needed to work the task.
    Show { task: String },
    /// The chain of intent from the root to a task: what an agent reads before working it.
    Context {
        task: String,
        /// Also print every ancestor's body, not just its narrative.
        #[arg(long)]
        full: bool,
    },
    /// The outline with derived status.
    Plan {
        /// Only this subtree.
        task: Option<String>,
        /// Also show complete and dropped tasks.
        #[arg(short, long)]
        all: bool,
        /// Only tasks with this field (`key` or `key=value`), own or inherited. Repeatable.
        #[arg(long = "where", value_name = "KEY[=VALUE]")]
        filters: Vec<String>,
        /// How many levels below the task (or the roots) to show: 0 is the task alone.
        #[arg(long, value_name = "N")]
        depth: Option<usize>,
        /// Under each task, one line: the part it takes of its parent (its fit), else the
        /// first line of its narrative or body. The placement view.
        #[arg(long)]
        scope: bool,
    },
    /// One level, flat: the roots, or a task's children, each with a count of the leaves under it.
    Ls {
        task: Option<String>,
        /// Also show complete and dropped tasks.
        #[arg(short, long)]
        all: bool,
        /// Only tasks with this field (`key` or `key=value`), own or inherited. Repeatable.
        #[arg(long = "where", value_name = "KEY[=VALUE]")]
        filters: Vec<String>,
        /// Under each task, one line: the part it takes of its parent (its fit), else the
        /// first line of its narrative or body. What each child already takes, before
        /// claiming a part.
        #[arg(long)]
        scope: bool,
    },
    /// Leaves that can start now, what a date is holding back, and what every other open
    /// leaf waits on. Ongoing tasks never appear.
    Ready {
        /// Also list blocked leaves.
        #[arg(short, long)]
        blocked: bool,
        /// Only tasks with this field (`key` or `key=value`), own or inherited. Repeatable.
        #[arg(long = "where", value_name = "KEY[=VALUE]")]
        filters: Vec<String>,
        /// Under each ready task, what it holds up: the direct successors of it and its
        /// ancestors.
        #[arg(long)]
        holds: bool,
    },
    /// Every dependency loop, with the requirements that form it.
    Loops,
    /// Open tasks by requester and date, for the stale-or-irrelevant review.
    Stale,
    /// Every forced start or completion.
    Forced,
    /// Events, newest first; for one task if given.
    Log {
        task: Option<String>,
        #[arg(short = 'n', long)]
        limit: Option<usize>,
    },
    /// What changed: in the whole plan between two events (default: since the last git
    /// commit of the store, else the last event), or in one task.
    Diff {
        task: Option<String>,
        /// The older event.
        #[arg(long)]
        from: Option<String>,
        /// The newer event (default HEAD).
        #[arg(long)]
        to: Option<String>,
        /// Take the older event from the store's HEAD as committed in git at REV (default HEAD).
        #[arg(long, value_name = "REV", num_args = 0..=1, default_missing_value = "HEAD")]
        git: Option<String>,
    },
    /// Write a markdown page per task into a folder a vault can link to and browse.
    Render {
        /// Output folder, relative to the project (default: config `render.dir`, else `tasks`).
        #[arg(long)]
        dir: Option<String>,
    },
    /// Markdown files in the project that link to a task's page.
    Backlinks { task: String },
    /// Remove objects nothing reaches; with --before, also the snapshots of older events,
    /// keeping every event and the log.
    Prune {
        /// Events older than this one lose their snapshots.
        #[arg(long, value_name = "EVENT")]
        before: Option<String>,
        /// Report what would go without removing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Replay another line of history (another copy of this store) onto ours.
    Merge {
        /// The other HEAD. Default: the merge git left pending or conflicted in `.tasks/HEAD`.
        theirs: Option<String>,
        /// Resolve a field changed on both sides: `ours` or `theirs`.
        #[arg(long, value_name = "SIDE")]
        prefer: Option<String>,
        /// Report what would happen without writing.
        #[arg(long)]
        dry_run: bool,
        /// Take the other HEAD from git's MERGE_HEAD.
        #[arg(long)]
        git: bool,
    },
    /// Configure git to hand `.tasks/HEAD` merges to tsk (once per machine).
    GitSetup,
    /// Every move of HEAD on this copy, newest first: the way back from any merge or reset.
    Reflog {
        #[arg(short = 'n', long)]
        limit: Option<usize>,
    },
    /// Move HEAD to an earlier (or any) event, recorded in the reflog. Nothing is deleted.
    Reset { event: String },
    /// Read or set config (`author`, `editor`, `render.dir`, `lock.timeout`, `lock.stale`,
    /// `color.ui`, `require.by`).
    Config { key: Option<String>, value: Option<String> },

    // ----- plumbing -----
    /// Print an object by hash.
    CatObject { hash: String },
    /// Hash stdin or a file as tsk would, without storing it.
    HashObject { file: Option<String> },
    /// List every id -> hash in a snapshot (default HEAD).
    LsSnapshot { event: Option<String> },
    /// The git merge driver for `.tasks/HEAD` (installed by `git-setup`).
    MergeDriver { ours: String, theirs: String },
    /// List event hashes from HEAD back.
    RevList {
        #[arg(short = 'n', long)]
        limit: Option<usize>,
    },
}

#[derive(Subcommand)]
enum TypeCmd {
    /// Create a type.
    Add {
        id: String,
        title: String,
        #[arg(short, long, allow_hyphen_values = true)]
        body: Option<String>,
        /// A document the type stands for (a process, a skill); instances inherit it. Repeatable.
        #[arg(long = "doc", value_name = "PATH")]
        docs: Vec<String>,
    },
    /// Add an item to a type. Applies to every task carrying the type, which `tsk type affected` lists.
    Item {
        r#type: String,
        id: String,
        title: String,
        #[arg(short, long, allow_hyphen_values = true)]
        body: Option<String>,
        /// The item is tentative: its existence waits on this task.
        #[arg(long)]
        pending: Option<String>,
        /// A requirement the item places: `--req "core: the command must exist first"`,
        /// `--req "item:cmd-2: ..."`, or `--req "1.2.3: ..."`. Blocking unless prefixed `~`.
        #[arg(long = "req")]
        reqs: Vec<String>,
        /// When the item applies; an instance that it doesn't apply to is dropped with that reason.
        #[arg(long, allow_hyphen_values = true)]
        when: Option<String>,
        /// A document the item's scope comes from; instances inherit it. Repeatable.
        #[arg(long = "doc", value_name = "PATH")]
        docs: Vec<String>,
    },
    /// Change an item's title or body on the type; instances are listed for review.
    EditItem {
        r#type: String,
        item: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(short, long, allow_hyphen_values = true)]
        body: Option<String>,
    },
    /// Show a type and the tasks that carry it.
    Show { r#type: String },
    /// List types.
    List,
}

/// The command runs on a thread with room to spare. In a debug build clap
/// builds the whole command tree on the stack, and with 0.5.0's tree that
/// passed the 1 MB a Windows main thread gets (every CI test on
/// windows-latest overflowed at start-up, 2026-11-04); the GNU toolchain
/// used for development gives more and never showed it. Reserving 16 MB
/// costs nothing until it is used.
const STACK: usize = 16 << 20;

fn main() {
    let t = std::thread::Builder::new().stack_size(STACK).spawn(run).expect("spawning the command thread");
    if t.join().is_err() {
        // A panic: its message has been printed; exit as a panic would.
        std::process::exit(101);
    }
}

fn run() {
    let cli = Cli::parse();
    let json = cli.json;
    commands::set_json(json);
    if let Some(d) = &cli.chdir {
        if let Err(e) = std::env::set_current_dir(d) {
            eprintln!("tsk: cannot change to {}: {}", d.display(), e);
            std::process::exit(1);
        }
    }
    commands::set_store(cli.store.clone());
    // Colour is decided once, before any output; the store's `color.ui` is
    // read if there is a store to read it from.
    let color_cfg = commands::store_open().ok().and_then(|s| s.config_value("color.ui"));
    if let Err(e) = color::init(cli.color.as_deref(), color_cfg.as_deref(), json) {
        eprintln!("tsk: {}", e);
        std::process::exit(1);
    }
    let r = match cli.cmd {
        Cmd::Init => commands::init(),
        Cmd::Add { title, body, required_by, priority, due, after, ongoing } => commands::add(title, body, required_by, priority, due, after, ongoing),
        Cmd::Breakdown { task, file, template } => commands::breakdown(task, file, template),
        Cmd::Amend { parent, title, body, fit, narrative, after, due, core, core_body } => commands::amend(parent, title, body, fit, narrative, after, due, core, core_body),
        Cmd::Adopt { parent, tasks, fit, narrative, core, core_body } => commands::adopt(parent, tasks, fit, narrative, core, core_body),
        Cmd::Edit { task, file, body, title, narrative, append, fit } => commands::edit(task, file, body, title, narrative, append, fit),
        Cmd::Ongoing { task, admits, off } => commands::ongoing(task, admits, off),
        Cmd::Close { task, reason } => commands::close(task, reason),
        Cmd::Reopen { task } => commands::reopen(task),
        Cmd::When { task, after, due, clear, why } => commands::when(task, after, due, clear, why),
        Cmd::Link { pred, succ, reason, conditional } => commands::link(pred, succ, reason, conditional),
        Cmd::Require { from, on, reason } => commands::require(from, on, reason),
        Cmd::By { task, person, reason } => commands::by(task, person, reason),
        Cmd::Move { req, from, on } => commands::move_req(req, from, on),
        Cmd::Unlink { req } => commands::unlink(req),
        Cmd::Set { target, pairs, unset, req } => commands::set(target, pairs, unset, req),
        Cmd::Doc { task, path, rm } => commands::doc(task, path, rm),
        Cmd::Type(t) => match t {
            TypeCmd::Add { id, title, body, docs } => commands::type_add(id, title, body, docs),
            TypeCmd::Item { r#type, id, title, body, pending, reqs, when, docs } => commands::type_item(r#type, id, title, body, pending, reqs, when, docs),
            TypeCmd::EditItem { r#type, item, title, body } => commands::type_edit_item(r#type, item, title, body),
            TypeCmd::Show { r#type } => commands::type_show(r#type, json),
            TypeCmd::List => commands::type_list(json),
        },
        Cmd::Apply { r#type, task } => commands::apply(r#type, task),
        Cmd::Spike { task } => commands::spike(task),
        Cmd::Confirm { task } => commands::confirm(task),
        Cmd::Start { tasks, force } => commands::start(tasks, force),
        Cmd::Pause { task } => commands::pause(task),
        Cmd::Complete { tasks, force } => commands::complete(tasks, force),
        Cmd::Uncomplete { task } => commands::uncomplete(task),
        Cmd::Drop { task, reason } => commands::drop(task, reason),
        Cmd::Show { task } => views::show(task, json),
        Cmd::Context { task, full } => views::context(task, full, json),
        Cmd::Plan { task, all, filters, depth, scope } => views::plan(task, all, filters, depth, scope, json),
        Cmd::Ls { task, all, filters, scope } => views::ls(task, all, filters, scope, json),
        Cmd::Ready { blocked, filters, holds } => views::ready(blocked, filters, holds, json),
        Cmd::Loops => views::loops(json),
        Cmd::Stale => views::stale(json),
        Cmd::Forced => views::forced(json),
        Cmd::Log { task, limit } => views::log(task, limit, json),
        Cmd::Diff { task, from, to, git } => views::diff(task, from, to, git, json),
        Cmd::Render { dir } => render::render(dir),
        Cmd::Backlinks { task } => render::backlinks(task, json),
        Cmd::Prune { before, dry_run } => prune::prune(before, dry_run),
        Cmd::Merge { theirs, prefer, dry_run, git } => merge::merge(theirs, prefer, dry_run, git),
        Cmd::GitSetup => merge::git_setup(),
        Cmd::Reflog { limit } => commands::reflog(limit, json),
        Cmd::Reset { event } => commands::reset(event),
        Cmd::MergeDriver { ours, theirs } => merge::merge_driver(ours, theirs),
        Cmd::Config { key, value } => commands::config(key, value),
        Cmd::CatObject { hash } => commands::cat_object(hash),
        Cmd::HashObject { file } => commands::hash_object(file),
        Cmd::LsSnapshot { event } => commands::ls_snapshot(event),
        Cmd::RevList { limit } => commands::rev_list(limit),
    };
    if let Err(e) = r {
        eprintln!("tsk: {:#}", e);
        // 3: the store moved under the command; it is safe to run again.
        std::process::exit(if e.downcast_ref::<store::StoreMoved>().is_some() { 3 } else { 1 });
    }
}
