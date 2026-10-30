mod breakdown;
mod commands;
mod commit;
mod model;
mod plan;
mod state;
mod store;

use clap::{Parser, Subcommand};

/// A simple CLI for project planning and tracking. Command language inspired
/// by Git: every change is an immutable event, every state is derived.
#[derive(Parser)]
#[command(name = "tsk", version, about, max_term_width = 100)]
struct Cli {
    /// Machine-readable output: views print JSON, and a change prints its event.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a `.tasks/` store in the current directory.
    Init,
    /// Add a root task. Children are added only by `breakdown` or `amend`.
    Add {
        title: String,
        /// Card body (markdown). `-` reads stdin.
        #[arg(short, long)]
        body: Option<String>,
        /// Who requires it, and why: `--by "Dana: the same two picks always win"`.
        #[arg(long = "by")]
        required_by: Vec<String>,
        #[arg(long)]
        priority: Option<String>,
        #[arg(long)]
        due: Option<String>,
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
    /// Add a child to a task that is already broken down, with the scope it covers.
    Amend {
        parent: String,
        title: String,
        #[arg(short, long)]
        body: Option<String>,
        /// Text appended to the parent's narrative saying what the child covers.
        #[arg(long)]
        narrative: Option<String>,
    },
    /// Edit a task's body in $EDITOR, or set it from a file or argument.
    Edit {
        task: String,
        #[arg(short = 'F', long)]
        file: Option<String>,
        #[arg(short, long)]
        body: Option<String>,
        #[arg(long)]
        title: Option<String>,
        /// Replace the breakdown narrative.
        #[arg(long)]
        narrative: Option<String>,
    },
    /// Make PRED a predecessor of SUCC, for a reason. The reason is what SUCC requires of PRED.
    Link {
        pred: String,
        succ: String,
        reason: String,
        /// SUCC's existence, not just its start, waits on PRED (a tentative task).
        #[arg(long)]
        conditional: bool,
    },
    /// Place a technical requirement FROM one task ON another. Not an ordering.
    Require {
        from: String,
        on: String,
        reason: String,
    },
    /// Record that a person requires a task, and why.
    By {
        task: String,
        person: String,
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
    Drop { task: String, reason: String },
    /// The card: everything needed to work the task.
    Show { task: String },
    /// The outline with derived status.
    Plan {
        /// Only this subtree.
        task: Option<String>,
        /// Also show complete and dropped tasks.
        #[arg(short, long)]
        all: bool,
    },
    /// Leaves that can start now, and what every other open leaf waits on.
    Ready {
        /// Also list blocked leaves.
        #[arg(short, long)]
        blocked: bool,
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
    /// What changed in a task between two events (default: its last change).
    Diff {
        task: String,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        to: Option<String>,
    },
    /// Read or set config (`author`, `editor`, aliases as `alias.<name>`).
    Config { key: Option<String>, value: Option<String> },

    // ----- plumbing -----
    /// Print an object by hash.
    CatObject { hash: String },
    /// Hash stdin or a file as tsk would, without storing it.
    HashObject { file: Option<String> },
    /// List every id -> hash in a snapshot (default HEAD).
    LsSnapshot { event: Option<String> },
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
        #[arg(short, long)]
        body: Option<String>,
    },
    /// Add an item to a type. Applies to every task carrying the type, which `tsk type affected` lists.
    Item {
        r#type: String,
        id: String,
        title: String,
        #[arg(short, long)]
        body: Option<String>,
        /// The item is tentative: its existence waits on this task.
        #[arg(long)]
        pending: Option<String>,
        /// A requirement the item places: `--req "core: the command must exist first"`,
        /// `--req "item:cmd-2: ..."`, or `--req "1.2.3: ..."`. Blocking unless prefixed `~`.
        #[arg(long = "req")]
        reqs: Vec<String>,
    },
    /// Change an item's title or body on the type; instances are listed for review.
    EditItem {
        r#type: String,
        item: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(short, long)]
        body: Option<String>,
    },
    /// Show a type and the tasks that carry it.
    Show { r#type: String },
    /// List types.
    List,
}

fn main() {
    let cli = Cli::parse();
    let json = cli.json;
    commands::set_json(json);
    let r = match cli.cmd {
        Cmd::Init => commands::init(),
        Cmd::Add { title, body, required_by, priority, due } => commands::add(title, body, required_by, priority, due),
        Cmd::Breakdown { task, file, template } => commands::breakdown(task, file, template),
        Cmd::Amend { parent, title, body, narrative } => commands::amend(parent, title, body, narrative),
        Cmd::Edit { task, file, body, title, narrative } => commands::edit(task, file, body, title, narrative),
        Cmd::Link { pred, succ, reason, conditional } => commands::link(pred, succ, reason, conditional),
        Cmd::Require { from, on, reason } => commands::require(from, on, reason),
        Cmd::By { task, person, reason } => commands::by(task, person, reason),
        Cmd::Move { req, from, on } => commands::move_req(req, from, on),
        Cmd::Unlink { req } => commands::unlink(req),
        Cmd::Type(t) => match t {
            TypeCmd::Add { id, title, body } => commands::type_add(id, title, body),
            TypeCmd::Item { r#type, id, title, body, pending, reqs } => commands::type_item(r#type, id, title, body, pending, reqs),
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
        Cmd::Show { task } => commands::show(task, json),
        Cmd::Plan { task, all } => commands::plan(task, all, json),
        Cmd::Ready { blocked } => commands::ready(blocked, json),
        Cmd::Loops => commands::loops(json),
        Cmd::Stale => commands::stale(json),
        Cmd::Forced => commands::forced(json),
        Cmd::Log { task, limit } => commands::log(task, limit, json),
        Cmd::Diff { task, from, to } => commands::diff(task, from, to),
        Cmd::Config { key, value } => commands::config(key, value),
        Cmd::CatObject { hash } => commands::cat_object(hash),
        Cmd::HashObject { file } => commands::hash_object(file),
        Cmd::LsSnapshot { event } => commands::ls_snapshot(event),
        Cmd::RevList { limit } => commands::rev_list(limit),
    };
    if let Err(e) = r {
        eprintln!("tsk: {}", e);
        std::process::exit(1);
    }
}
