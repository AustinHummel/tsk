//! Porcelain that changes the plan, and plumbing. Each porcelain command
//! takes the store lock, loads the plan, changes it in memory, and writes
//! exactly one event. Views live in `views.rs`.

use crate::breakdown;
use crate::commit::{self, Commit};
use crate::model::*;
use crate::plan::Plan;
use crate::state::{Block, Derived, Status};
use crate::store::{Lock, Store};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::json;
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub(crate) struct Ctx {
    pub store: Store,
    pub plan: Plan,
    /// The event the plan was loaded from: the parent of any event this
    /// command writes.
    pub head: Option<Hash>,
    /// Per-leaf statuses HEAD implies (from the cache, or the history).
    pub leaf: std::collections::HashMap<TaskId, Status>,
    pub derived: Derived,
    /// The whole event history, read only by the commands that show it.
    history: OnceLock<Vec<(Hash, Event)>>,
    /// Held for the life of a writing command.
    _lock: Option<Lock>,
}

impl Ctx {
    /// Write one event on top of the HEAD this context was loaded from.
    pub fn commit(&self, c: Commit) -> Result<Hash> {
        commit::write_on(&self.store, &self.plan, self.head.clone(), c)
    }

    /// Every event, newest first. Read on first use.
    pub fn history(&self) -> &[(Hash, Event)] {
        self.history.get_or_init(|| self.store.history().unwrap_or_default())
    }
}

static STORE: OnceLock<Option<PathBuf>> = OnceLock::new();

pub fn set_store(p: Option<PathBuf>) {
    let _ = STORE.set(p);
}

/// The store named by `--store` / `TSK_STORE`, else the one found by walking up.
pub(crate) fn store_open() -> Result<Store> {
    let store = match STORE.get().cloned().flatten() {
        Some(p) => Store::at(&p)?,
        None => Store::discover(&std::env::current_dir()?)?,
    };
    store.check_format()?;
    Ok(store)
}

fn load(store: Store, lock: Option<Lock>) -> Result<Ctx> {
    let (head, plan, leaf) = match store.head_event()? {
        None => (None, Plan::default(), std::collections::HashMap::new()),
        Some((h, ev)) => (Some(h.clone()), store.load_plan(&ev.snapshot)?, store.leaf_statuses_at(&h)?),
    };
    // The one read of the clock: every date in this command is compared
    // against this moment.
    let derived = Derived::compute(&plan, &leaf, crate::clock::now()?);
    Ok(Ctx { store, plan, head, leaf, derived, history: OnceLock::new(), _lock: lock })
}

/// Read-only: no lock. Readers see HEAD and its snapshot, both immutable.
pub(crate) fn open() -> Result<Ctx> {
    let store = store_open()?;
    crate::merge::settle_pending(&store, false)?;
    load(store, None)
}

/// For a change: take the lock first, then load, so the plan in memory is
/// the one the event will follow.
pub(crate) fn open_rw() -> Result<Ctx> {
    let store = store_open()?;
    crate::merge::settle_pending(&store, true)?;
    let lock = store.lock()?;
    load(store, Some(lock))
}

pub(crate) fn read_text(arg: Option<String>, from_file: bool) -> Result<Option<String>> {
    match arg {
        None => Ok(None),
        Some(s) if s == "-" => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            Ok(Some(buf))
        }
        Some(s) if from_file => Ok(Some(std::fs::read_to_string(&s).with_context(|| format!("reading {}", s))?)),
        Some(s) => Ok(Some(s)),
    }
}

fn editor_cmd(store: &Store) -> String {
    store
        .config_value("editor")
        .or_else(|| std::env::var("TSK_EDITOR").ok())
        .or_else(|| std::env::var("VISUAL").ok())
        .or_else(|| std::env::var("EDITOR").ok())
        .unwrap_or_else(|| if cfg!(windows) { "notepad".to_string() } else { "vi".to_string() })
}

/// Open `initial` in the editor and return what was saved. Refuses when
/// there is no terminal to hand the editor to, so a script or an agent
/// fails fast instead of hanging on a window it cannot see. Runs without
/// the store lock: the edit can take as long as it takes.
fn edit_in_editor(store: &Store, name: &str, initial: &str) -> Result<String> {
    use std::io::IsTerminal;
    let cmd = editor_cmd(store);
    if !std::io::stdin().is_terminal() || cmd == "none" {
        bail!(
            "no terminal for an editor; pass the content with `-F <file>` or `-F -` (stdin)\n\
             (`tsk breakdown <task> --template` prints the form to fill in)"
        );
    }
    let path = store.dir.join(format!("{}-{}.EDIT", name, std::process::id()));
    std::fs::write(&path, initial)?;
    let mut parts = cmd.split_whitespace();
    let exe = parts.next().unwrap_or("notepad");
    let status = std::process::Command::new(exe)
        .args(parts)
        .arg(&path)
        .status()
        .with_context(|| format!("launching editor `{}`", cmd))?;
    if !status.success() {
        bail!("editor exited with {}", status);
    }
    let text = std::fs::read_to_string(&path)?;
    let _ = std::fs::remove_file(&path);
    Ok(text)
}

fn parse_by(s: &str) -> Result<(String, String)> {
    let (p, r) = s
        .split_once(':')
        .ok_or_else(|| anyhow!("`--by` wants `Person: reason`, got `{}`", s))?;
    Ok((p.trim().to_string(), r.trim().to_string()))
}

pub(crate) fn req_desc(plan: &Plan, r: &Requirement) -> String {
    if let Requirer::Person(p) = &r.from {
        return format!("required by [{}] on [{}]: {}", p, plan.label(&r.on), r.reason);
    }
    let kind = if r.conditional {
        "conditional"
    } else if r.blocking {
        "blocking"
    } else {
        "technical"
    };
    format!("{} on [{}] from [{}]: {}", kind, plan.label(&r.on), plan.requirer_label(&r.from), r.reason)
}

static JSON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_json(on: bool) {
    JSON.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn json_mode() -> bool {
    JSON.load(std::sync::atomic::Ordering::Relaxed)
}

/// Human text, suppressed in `--json` mode.
pub(crate) fn say(s: &str) {
    if !json_mode() {
        println!("{}", s);
    }
}

/// The result of a mutating command: one line with the event hash, or in
/// `--json` mode the event itself (kind, subjects, group, forced, message).
pub(crate) fn done(store: &Store, hash: &Hash, msg: &str) {
    if !json_mode() {
        println!("{}  {}", msg, crate::color::dim(&format!("({})", &hash[..10])));
        return;
    }
    let v = match store.read_object::<Event>(ObjectType::Event, hash) {
        Ok(e) => json!({"event": hash, "kind": e.kind.name(), "message": e.message, "subjects": e.subjects, "group": e.group, "forced": e.forced, "time": e.time, "author": e.author}),
        Err(_) => json!({"event": hash, "message": msg}),
    };
    println!("{}", serde_json::to_string_pretty(&v).unwrap());
}

fn new_task(plan: &mut Plan, title: &str, parent: Option<TaskId>, body: String) -> TaskId {
    let id = plan.new_task_id();
    let seq = plan.next_task;
    plan.tasks.insert(
        id.clone(),
        Task {
            id: id.clone(),
            title: title.to_string(),
            parent,
            seq,
            body,
            types: vec![],
            from_item: None,
            core_of: None,
            narrative: None,
            fit: None,
            ongoing: false,
            after: None,
            priority: None,
            due: None,
            docs: vec![],
            fields: Fields::new(),
            former: vec![],
        },
    );
    id
}

// ===== porcelain ===========================================================

pub fn init() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let store = Store::init(&cwd)?;
    let plan = Plan::default();
    let h = commit::write_on(&store, &plan, None, Commit::new(EventKind::Init, "init", vec![]))?;
    done(&store, &h, &format!("initialized {}", store.dir.display()));
    Ok(())
}

/// A `--after`: a date, or a local time stored with its offset.
fn parse_after(after: Option<String>) -> Result<Option<String>> {
    after.map(|a| crate::clock::normalize(&a, "--after")).transpose()
}

#[allow(clippy::too_many_arguments)]
pub fn add(title: String, body: Option<String>, by: Vec<String>, priority: Option<String>, due: Option<String>, after: Option<String>, ongoing: Option<String>) -> Result<()> {
    let body = read_text(body, false)?.unwrap_or_default();
    let after = parse_after(after)?;
    let mut c = open_rw()?;
    let require_by = c.store.config_value("require.by").map(|v| matches!(v.trim(), "true" | "1" | "yes")).unwrap_or(false);
    if by.is_empty() && require_by {
        bail!("every root needs --by here (config require.by)");
    }
    let id = new_task(&mut c.plan, &title, None, body);
    {
        let t = c.plan.tasks.get_mut(&id).unwrap();
        t.priority = priority;
        t.due = due.map(|d| crate::clock::normalize_due(&d));
        t.after = after;
        if let Some(admits) = &ongoing {
            t.ongoing = true;
            t.narrative = Some(admits.trim().to_string());
        }
    }
    let noted = by.is_empty();
    for b in by {
        let (p, r) = parse_by(&b)?;
        add_req(&mut c, Requirer::Person(p), id.clone(), r, false, false, None);
    }
    let h = c.commit(Commit::new(EventKind::Created, format!("add {}", title), vec![id.clone()]))?;
    done(&c.store, &h, &format!("created {}{}", c.plan.label(&id), if ongoing.is_some() { "  <ongoing>" } else { "" }));
    if noted {
        say(&format!("nobody has asked for this; tsk by {} <person> \"<why>\" records who did", id));
    }
    Ok(())
}

fn check_open(c: &Ctx, id: &str) -> Result<()> {
    if c.derived.is_closed(&c.plan, &c.leaf, id) {
        bail!("{} is closed; `tsk reopen` it first", c.plan.label(id));
    }
    match c.derived.status(id) {
        Status::Complete => bail!("{} is complete; `tsk uncomplete` it first", c.plan.label(id)),
        Status::Dropped => bail!("{} is dropped", c.plan.label(id)),
        _ => Ok(()),
    }
}

fn check_leaf_open(c: &Ctx, id: &str, verb: &str) -> Result<()> {
    if c.plan.is_parent(id) {
        bail!("{} is already broken down; {} one of its children instead", c.plan.label(id), verb);
    }
    check_open(c, id)
}

fn check_not_ongoing(c: &Ctx, id: &str) -> Result<()> {
    if c.plan.tasks.get(id).map(|t| t.ongoing).unwrap_or(false) {
        bail!("{} is ongoing: it has no end of its own; add the work under it (tsk amend), or tsk close it when the scope is shut", c.plan.label(id));
    }
    Ok(())
}

fn first_line(s: &str) -> &str {
    s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim()
}

/// What amend and adopt check before a parent takes scope: it is open and
/// not closed, and the `--fit` is given and not already a sibling's.
fn parent_takes(c: &Ctx, pid: &str, fit: Option<String>) -> Result<String> {
    let label = c.plan.label(pid);
    let fit = fit.map(|f| f.trim().to_string()).filter(|f| !f.is_empty());
    let Some(fit) = fit else {
        bail!("--fit is required: say which part of {}'s scope this child takes, so a reader can tell it from its siblings", label);
    };
    if c.derived.is_closed(&c.plan, &c.leaf, pid) {
        bail!("{} is closed; tsk reopen it first", label);
    }
    match c.derived.status(pid) {
        Status::Complete => bail!("{} is complete; a complete task's scope doesn't grow: file this elsewhere, or tsk uncomplete one of its children first", label),
        Status::Dropped => bail!("{} is dropped", label),
        _ => {}
    }
    let lower = fit.to_lowercase();
    for k in c.plan.children(pid) {
        if c.derived.status(&k.id) == Status::Dropped {
            continue;
        }
        if k.fit.as_deref().map(|f| f.trim().to_lowercase() == lower).unwrap_or(false) {
            bail!("{} already takes that part; two children never take the same piece of a scope", c.plan.label(&k.id));
        }
    }
    Ok(fit)
}

/// A leaf that gains a child is cut in two by the person: `--core` names
/// the piece that holds the leaf's own work, which takes its body (or
/// `--core-body`) and a generated fit. An ongoing childless task has no
/// scope of its own to keep, so it is not split. Returns the core child.
fn split_leaf(c: &mut Ctx, pid: &TaskId, core: Option<String>, core_body: Option<String>, new_titles: &[String]) -> Result<Option<TaskId>> {
    let label = c.plan.label(pid);
    if c.plan.is_parent(pid) {
        if core.is_some() {
            bail!("--core names the piece a childless parent keeps; {} already has children", label);
        }
        return Ok(None);
    }
    let host = c.plan.task(pid)?.clone();
    if host.ongoing {
        if core.is_some() {
            bail!("{} is ongoing and has no scope of its own to keep; drop --core", label);
        }
        return Ok(None);
    }
    let core = core.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    let Some(core) = core else {
        bail!("{} has no children yet; its own work becomes a piece beside the new one: name it with --core \"<title>\"", label);
    };
    let body = match core_body {
        Some(b) => read_text(Some(b), false)?.unwrap_or_default(),
        None => host.body.clone(),
    };
    let cid = new_task(&mut c.plan, &core, Some(pid.clone()), body);
    let t = c.plan.tasks.get_mut(&cid).unwrap();
    t.core_of = Some(pid.clone());
    t.fit = Some(format!("what {} was before {} was split out", host.title, new_titles.join(", ")));
    Ok(Some(cid))
}

/// The parent's narrative after it took scope: replaced when given;
/// generated from the two pieces after a split; otherwise left as it is.
fn settle_narrative(c: &mut Ctx, pid: &str, narrative: Option<String>, core: Option<&TaskId>, new_titles: &[String], fit: &str) {
    let text = match (narrative, core) {
        (Some(n), _) => Some(n),
        (None, Some(core)) => {
            let ct = c.plan.tasks[core].title.clone();
            Some(if new_titles.len() == 1 { format!("Two pieces: {}, and {} ({}).", ct, new_titles[0], fit) } else { format!("Pieces: {}; and {} ({}).", ct, new_titles.join(", "), fit) })
        }
        (None, None) => None,
    };
    if let Some(n) = text {
        c.plan.tasks.get_mut(pid).unwrap().narrative = Some(n);
    }
}

fn say_admits(c: &Ctx, pid: &str) {
    let t = &c.plan.tasks[pid];
    if t.ongoing {
        say(&format!("  {} admits: {}", c.plan.label(pid), first_line(t.narrative.as_deref().unwrap_or(""))));
    }
}

/// Blocking requirements from tasks that sit on `id` or any ancestor:
/// what waits on this task (successor label, requirement id).
fn successors_of(c: &Ctx, id: &str) -> Vec<(String, ReqId)> {
    let mut v = Vec::new();
    for a in c.plan.ancestors_and_self(id) {
        for r in c.plan.reqs_on(&a.id) {
            if let (true, Requirer::Task(f)) = (r.blocking, &r.from) {
                v.push((c.plan.label(f), r.id.clone()));
            }
        }
    }
    v
}

/// A live ongoing task at or under `id`, if any.
fn ongoing_under(c: &Ctx, id: &str) -> Option<TaskId> {
    c.plan.subtree(id).into_iter().find(|d| c.plan.tasks[d].ongoing && c.derived.status(d) != Status::Dropped)
}

/// What a breakdown of `task` must disposition, and the template for it.
fn breakdown_prep(c: &Ctx, task: &str) -> Result<(TaskId, Vec<(String, String)>, Vec<(String, String)>, String)> {
    let id = c.plan.resolve(task)?;
    check_leaf_open(c, &id, "break down")?;
    let parent = c.plan.task(&id)?;
    // Every requirement between tasks that touches the parent must be
    // dispositioned. A person's requirement may be, and stays if it isn't.
    let mut to_place: Vec<(String, String)> = Vec::new();
    let mut optional: Vec<(String, String)> = Vec::new();
    for r in c.plan.reqs_on(&id) {
        if matches!(r.from, Requirer::Task(_)) {
            to_place.push((r.id.clone(), req_desc(&c.plan, r)));
        } else {
            optional.push((r.id.clone(), req_desc(&c.plan, r)));
        }
    }
    for r in c.plan.reqs_from(&id) {
        to_place.push((r.id.clone(), req_desc(&c.plan, r)));
    }
    let tmpl = breakdown::template(&c.plan.label(&id), &parent.body, &to_place, &optional);
    Ok((id, to_place, optional, tmpl))
}

pub fn breakdown(task: String, file: Option<String>, template: bool) -> Result<()> {
    // Everything that may take a while (reading stdin, the editor) happens
    // before the lock is taken.
    let given = read_text(file, true)?;
    let text = match (given, template) {
        (_, true) => {
            let c = open()?;
            let (_, _, _, tmpl) = breakdown_prep(&c, &task)?;
            print!("{}", tmpl);
            return Ok(());
        }
        (Some(t), false) => t,
        (None, false) => {
            let c = open()?;
            let (_, _, _, tmpl) = breakdown_prep(&c, &task)?;
            edit_in_editor(&c.store, "BREAKDOWN", &tmpl)?
        }
    };
    let mut c = open_rw()?;
    let (id, to_place, optional, _) = breakdown_prep(&c, &task)?;
    let bd = breakdown::parse(&text)?;
    if bd.narrative.is_empty() {
        bail!("the breakdown needs a `= narrative`: how the children together fulfill {}", c.plan.label(&id));
    }
    let missing: Vec<&str> = to_place.iter().filter(|(r, _)| !bd.dispositions.contains_key(r)).map(|(r, _)| r.as_str()).collect();
    if !missing.is_empty() {
        bail!("every requirement on the parent must go to a child or `all`; missing: {}", missing.join(", "));
    }
    for r in bd.dispositions.keys() {
        if !to_place.iter().any(|(x, _)| x == r) && !optional.iter().any(|(x, _)| x == r) {
            bail!("requirement {} does not touch {}", r, c.plan.label(&id));
        }
    }

    let mut subjects = vec![id.clone()];
    let mut child_ids = Vec::new();
    for (title, body) in &bd.children {
        let cid = new_task(&mut c.plan, title, Some(id.clone()), body.clone());
        subjects.push(cid.clone());
        child_ids.push(cid);
    }
    for (rid, idx) in &bd.dispositions {
        if *idx == 0 {
            continue;
        }
        let target = child_ids[*idx - 1].clone();
        let r = c.plan.reqs.get_mut(rid).unwrap();
        if r.on == id {
            r.on = target;
        } else {
            r.from = Requirer::Task(target);
        }
    }
    c.plan.tasks.get_mut(&id).unwrap().narrative = Some(bd.narrative.clone());
    let n = child_ids.len();
    let h = c.commit(Commit::new(EventKind::BrokenDown, format!("breakdown {} into {} children", c.plan.number_of(&id), n), subjects))?;
    done(&c.store, &h, &format!("broke down {} into {} children", c.plan.label(&id), n));
    for cid in &child_ids {
        say(&format!("  {}", c.plan.label(cid)));
    }
    Ok(())
}

/// Add a child to a task, with the part of the parent's scope it takes
/// (`--fit`). A parent with no children yet is cut in two, and the person
/// names the piece that holds its own work (`--core`). `--narrative`
/// replaces the parent's; nothing is ever appended to one.
#[allow(clippy::too_many_arguments)]
pub fn amend(parent: String, title: String, body: Option<String>, fit: Option<String>, narrative: Option<String>, after: Option<String>, due: Option<String>, core: Option<String>, core_body: Option<String>) -> Result<()> {
    let body = read_text(body, false)?.unwrap_or_default();
    let after = parse_after(after)?;
    let mut c = open_rw()?;
    let pid = c.plan.resolve(&parent)?;
    let fit = parent_takes(&c, &pid, fit)?;
    let titles = vec![title.clone()];
    let core = split_leaf(&mut c, &pid, core, core_body, &titles)?;
    let cid = new_task(&mut c.plan, &title, Some(pid.clone()), body);
    {
        let t = c.plan.tasks.get_mut(&cid).unwrap();
        t.fit = Some(fit.clone());
        t.after = after;
        t.due = due.map(|d| crate::clock::normalize_due(&d));
    }
    settle_narrative(&mut c, &pid, narrative, core.as_ref(), &titles, &fit);
    let mut subjects = vec![pid.clone()];
    subjects.extend(core.iter().cloned());
    subjects.push(cid.clone());
    let h = c.commit(Commit::new(EventKind::Amended, format!("amend {} with {}", c.plan.number_of(&pid), title), subjects))?;
    done(&c.store, &h, &format!("added {} under {}", c.plan.label(&cid), c.plan.label(&pid)));
    if let Some(core) = core {
        say(&format!("  {} holds what {} was; {} is split out", c.plan.label(&core), c.plan.tasks[&pid].title, c.plan.label(&cid)));
    }
    say_admits(&c, &pid);
    Ok(())
}

/// Move existing tasks under a parent, keeping everything about them: id,
/// history, children, requirements, docs, fields. Each takes a new position
/// at the end of the parent's children (drawn from the task counter, so no
/// existing number moves) and records the part of the parent it takes
/// (`--fit`). A leaf parent is cut in two, as `amend` does.
#[allow(clippy::too_many_arguments)]
pub fn adopt(parent: String, tasks: Vec<String>, fit: Option<String>, narrative: Option<String>, core: Option<String>, core_body: Option<String>) -> Result<()> {
    if tasks.is_empty() {
        bail!("name at least one task to adopt");
    }
    let mut c = open_rw()?;
    let pid = c.plan.resolve(&parent)?;
    let mut ids: Vec<TaskId> = Vec::new();
    for t in &tasks {
        let id = c.plan.resolve(t)?;
        if ids.contains(&id) {
            bail!("{} is named twice", c.plan.label(&id));
        }
        ids.push(id);
    }
    let fit = parent_takes(&c, &pid, fit)?;
    for id in &ids {
        if *id == pid {
            bail!("{} cannot adopt itself", c.plan.label(id));
        }
        let t = c.plan.task(id)?;
        if t.parent.as_deref() == Some(pid.as_str()) {
            bail!("{} is already under {}", c.plan.label(id), c.plan.label(&pid));
        }
        if c.plan.subtree(id).contains(&pid) {
            bail!("{} is above {}: a task cannot become its own ancestor", c.plan.label(id), c.plan.label(&pid));
        }
        if t.core_of.is_some() || t.from_item.is_some() {
            bail!("{} is the core or an item of a typed task; it stays with its host", c.plan.label(id));
        }
        if ongoing_under(&c, id).is_some() {
            let succ = successors_of(&c, &pid);
            if !succ.is_empty() {
                bail!(
                    "{} is a predecessor of {} ({}); an ongoing task under it would make them wait until it is closed: move those links to the piece they wait on first",
                    c.plan.label(&pid),
                    succ.iter().map(|(l, _)| l.clone()).collect::<Vec<_>>().join("; "),
                    succ.iter().map(|(_, r)| r.clone()).collect::<Vec<_>>().join(", ")
                );
            }
        }
    }
    let titles: Vec<String> = ids.iter().map(|i| c.plan.tasks[i].title.clone()).collect();
    let mut subjects = vec![pid.clone()];
    let core = split_leaf(&mut c, &pid, core, core_body, &titles)?;
    subjects.extend(core.iter().cloned());
    let old_numbers: Vec<String> = ids.iter().map(|i| c.plan.number_of(i)).collect();
    for id in &ids {
        c.plan.next_task += 1;
        let seq = c.plan.next_task;
        let t = c.plan.tasks.get_mut(id).unwrap();
        // The old position stays vacant: its number is never reassigned.
        t.former.push(Former { parent: t.parent.clone(), seq: t.seq });
        t.parent = Some(pid.clone());
        t.seq = seq;
        t.fit = Some(fit.clone());
        subjects.push(id.clone());
    }
    c.plan.invalidate();
    settle_narrative(&mut c, &pid, narrative, core.as_ref(), &titles, &fit);
    let h = c.commit(Commit::new(EventKind::Adopted, format!("adopt {} under {}", old_numbers.join(" "), c.plan.number_of(&pid)), subjects))?;
    done(&c.store, &h, &format!("adopted under {}:", c.plan.label(&pid)));
    if let Some(core) = &core {
        say(&format!("  {} holds what {} was", c.plan.label(core), c.plan.tasks[&pid].title));
    }
    for (id, was) in ids.iter().zip(old_numbers) {
        say(&format!("  {}  (was {})", c.plan.label(id), was));
    }
    if ids.len() > 1 {
        say(&format!("  the same fit is on {} tasks; tsk edit <id> --fit gives each its own", ids.len()));
    }
    say_admits(&c, &pid);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn edit(task: String, file: Option<String>, body: Option<String>, title: Option<String>, narrative: Option<String>, append: Option<String>, fit: Option<String>) -> Result<()> {
    if append.is_some() && (file.is_some() || body.is_some()) {
        bail!("`--append` adds to the body; it cannot be combined with `-b` or `-F`");
    }
    let mut new_body = match (file, body) {
        (Some(f), _) => read_text(Some(f), true)?,
        (None, Some(b)) => read_text(Some(b), false)?,
        (None, None) => None,
    };
    if new_body.is_none() && title.is_none() && narrative.is_none() && append.is_none() && fit.is_none() {
        let c = open()?;
        let id = c.plan.resolve(&task)?;
        new_body = Some(edit_in_editor(&c.store, "CARD", &c.plan.task(&id)?.body)?);
    }
    let mut c = open_rw()?;
    let id = c.plan.resolve(&task)?;
    let mut t = c.plan.task(&id)?.clone();
    let mut changed = Vec::new();
    if let Some(n) = narrative {
        // A childless ongoing task's narrative is its admission test.
        if !c.plan.is_parent(&id) && !t.ongoing {
            bail!("{} has no children; the narrative is set by breakdown", c.plan.label(&id));
        }
        t.narrative = Some(n);
        changed.push("narrative");
    }
    if let Some(f) = fit {
        if t.parent.is_none() {
            bail!("{} is a root; it fits nothing above it", c.plan.label(&id));
        }
        t.fit = Some(f.trim().to_string());
        changed.push("fit");
    }
    if let Some(ti) = title {
        t.title = ti;
        changed.push("title");
    }
    if let Some(b) = new_body {
        if b.trim_end() != t.body.trim_end() {
            t.body = b.trim_end().to_string();
            changed.push("body");
        }
    }
    if let Some(a) = append {
        let a = a.trim();
        if a.is_empty() {
            bail!("nothing to append");
        }
        // A dated paragraph at the end; what was there stays.
        let mut b = t.body.trim_end().to_string();
        if !b.is_empty() {
            b.push_str("\n\n");
        }
        b.push_str(&format!("{}: {}", &commit::now()[..10], a));
        t.body = b;
        changed.push("appended to body");
    }
    if changed.is_empty() {
        say("nothing changed");
        return Ok(());
    }
    c.plan.tasks.insert(id.clone(), t);
    let h = c.commit(Commit::new(EventKind::Edited, format!("edit {} ({})", c.plan.number_of(&id), changed.join(", ")), vec![id.clone()]))?;
    done(&c.store, &h, &format!("edited {}: {}", c.plan.label(&id), changed.join(", ")));
    Ok(())
}

fn add_req(c: &mut Ctx, from: Requirer, on: TaskId, reason: String, blocking: bool, conditional: bool, from_item: Option<(TypeId, String)>) -> ReqId {
    let rid = c.plan.new_req_id();
    c.plan.reqs.insert(rid.clone(), Requirement { id: rid.clone(), from, on, reason, blocking, conditional, created: commit::now(), from_item, fields: Fields::new() });
    rid
}

fn note_loops(c: &Ctx, before: usize) -> Result<()> {
    let d = Derived::compute(&c.plan, &c.leaf, c.derived.now);
    if d.loops.len() > before {
        say("note: this forms a dependency loop; see `tsk loops`");
    }
    Ok(())
}

pub fn link(pred: String, succ: String, reason: String, conditional: bool) -> Result<()> {
    let mut c = open_rw()?;
    let p = c.plan.resolve(&pred)?;
    let s = c.plan.resolve(&succ)?;
    if p == s {
        bail!("a task cannot be its own predecessor");
    }
    check_pred_finite(&c, &p)?;
    let before = c.derived.loops.len();
    let rid = add_req(&mut c, Requirer::Task(s.clone()), p.clone(), reason.clone(), true, conditional, None);
    let h = c.commit(Commit::new(EventKind::Linked, format!("link {} -> {}: {}", c.plan.number_of(&p), c.plan.number_of(&s), reason), vec![p.clone(), s.clone()]))?;
    if conditional {
        done(&c.store, &h, &format!("{}: {} is tentative until {} is complete", rid, c.plan.label(&s), c.plan.label(&p)));
    } else {
        done(&c.store, &h, &format!("{}: {} is predecessor of {}", rid, c.plan.label(&p), c.plan.label(&s)));
    }
    if c.plan.is_parent(&p) {
        let n = c.derived.open_under(&c.plan, &p).len();
        say(&format!("note: {} has {} open under it; the link waits on all of them (tsk link <piece> ... waits on one)", c.plan.label(&p), n));
    }
    note_loops(&c, before)
}

/// A dependency on an ongoing task has no honest meaning: it never
/// completes on its own. The same holds when one sits under the predecessor.
fn check_pred_finite(c: &Ctx, pred: &str) -> Result<()> {
    if let Some(o) = ongoing_under(c, pred) {
        let which = if o == pred { "ongoing".to_string() } else { format!("ongoing: {}", c.plan.number_of(&o)) };
        bail!("{} never completes on its own ({}); link the piece of it you wait on, or the finite task that means it exists", c.plan.label(pred), which);
    }
    Ok(())
}

pub fn require(from: String, on: String, reason: String) -> Result<()> {
    let mut c = open_rw()?;
    let f = c.plan.resolve(&from)?;
    let o = c.plan.resolve(&on)?;
    if f == o {
        bail!("a task cannot require something of itself");
    }
    let rid = add_req(&mut c, Requirer::Task(f.clone()), o.clone(), reason.clone(), false, false, None);
    let h = c.commit(Commit::new(EventKind::Required, format!("require {} of {}: {}", c.plan.number_of(&f), c.plan.number_of(&o), reason), vec![f.clone(), o.clone()]))?;
    done(&c.store, &h, &format!("{}: {} requires of {}: {}", rid, c.plan.label(&f), c.plan.label(&o), reason));
    Ok(())
}

pub fn by(task: String, person: String, reason: String) -> Result<()> {
    let mut c = open_rw()?;
    let id = c.plan.resolve(&task)?;
    let rid = add_req(&mut c, Requirer::Person(person.clone()), id.clone(), reason.clone(), false, false, None);
    let h = c.commit(Commit::new(EventKind::Required, format!("{} requires {}: {}", person, c.plan.number_of(&id), reason), vec![id.clone()]))?;
    done(&c.store, &h, &format!("{}: {} required by {}", rid, c.plan.label(&id), person));
    Ok(())
}

pub fn move_req(req: String, from: Option<String>, on: Option<String>) -> Result<()> {
    let mut c = open_rw()?;
    let mut r = c.plan.reqs.get(&req).cloned().ok_or_else(|| anyhow!("no requirement {}", req))?;
    if from.is_none() && on.is_none() {
        bail!("say where it moves: `--from <task>` and/or `--on <task>`");
    }
    let mut subjects = vec![r.on.clone()];
    if let Requirer::Task(t) = &r.from {
        subjects.push(t.clone());
    }
    if let Some(f) = from {
        let fid = c.plan.resolve(&f)?;
        r.from = Requirer::Task(fid.clone());
        subjects.push(fid);
    }
    if let Some(o) = on {
        let oid = c.plan.resolve(&o)?;
        r.on = oid.clone();
        subjects.push(oid);
    }
    if let Requirer::Task(t) = &r.from {
        if *t == r.on {
            bail!("a task cannot require something of itself");
        }
    }
    if r.blocking {
        check_pred_finite(&c, &r.on)?;
    }
    let before = c.derived.loops.len();
    c.plan.reqs.insert(req.clone(), r.clone());
    let h = c.commit(Commit::new(EventKind::Moved, format!("move {}", req), subjects))?;
    done(&c.store, &h, &format!("moved {}: {}", req, req_desc(&c.plan, &r)));
    note_loops(&c, before)
}

pub fn unlink(req: String) -> Result<()> {
    let mut c = open_rw()?;
    let r = c.plan.reqs.remove(&req).ok_or_else(|| anyhow!("no requirement {}", req))?;
    let mut subjects = vec![r.on.clone()];
    if let Requirer::Task(t) = &r.from {
        subjects.push(t.clone());
    }
    let h = c.commit(Commit::new(EventKind::Unlinked, format!("unlink {}: {}", req, r.reason), subjects))?;
    done(&c.store, &h, &format!("removed {}", req));
    Ok(())
}

// ----- fields and docs ------------------------------------------------------

fn parse_pair(s: &str) -> Result<(String, String)> {
    let (k, v) = s.split_once('=').ok_or_else(|| anyhow!("`{}` is not `key=value`", s))?;
    let k = k.trim();
    if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) {
        bail!("field key `{}` must be letters, digits, `_`, `-` or `.`", k);
    }
    Ok((k.to_string(), v.trim().to_string()))
}

/// Apply `key=value` sets and `--unset` keys to a field map; returns what changed.
fn apply_fields(fields: &mut Fields, pairs: &[String], unset: &[String]) -> Result<Vec<String>> {
    let mut changed = Vec::new();
    for p in pairs {
        let (k, v) = parse_pair(p)?;
        if fields.get(&k) != Some(&v) {
            fields.insert(k.clone(), v.clone());
            changed.push(format!("{}={}", k, v));
        }
    }
    for k in unset {
        if fields.remove(k).is_some() {
            changed.push(format!("-{}", k));
        }
    }
    Ok(changed)
}

pub fn set(target: String, pairs: Vec<String>, unset: Vec<String>, req: bool) -> Result<()> {
    if pairs.is_empty() && unset.is_empty() {
        bail!("give `key=value` pairs and/or `--unset key`");
    }
    let mut c = open_rw()?;
    let (subjects, label, changed) = if req {
        let r = c.plan.reqs.get_mut(&target).ok_or_else(|| anyhow!("no requirement {}", target))?;
        let changed = apply_fields(&mut r.fields, &pairs, &unset)?;
        let mut subjects = vec![r.on.clone()];
        if let Requirer::Task(t) = &r.from {
            subjects.push(t.clone());
        }
        (subjects, target.clone(), changed)
    } else {
        let id = c.plan.resolve(&target)?;
        let t = c.plan.tasks.get_mut(&id).unwrap();
        let changed = apply_fields(&mut t.fields, &pairs, &unset)?;
        (vec![id.clone()], c.plan.number_of(&id), changed)
    };
    if changed.is_empty() {
        say("nothing changed");
        return Ok(());
    }
    let h = c.commit(Commit::new(EventKind::Set, format!("set {} {}", label, changed.join(" ")), subjects))?;
    done(&c.store, &h, &format!("set {}: {}", label, changed.join(" ")));
    Ok(())
}

pub fn doc(task: String, path: Option<String>, rm: Option<String>) -> Result<()> {
    if path.is_none() && rm.is_none() {
        let c = open()?;
        let id = c.plan.resolve(&task)?;
        let t = c.plan.task(&id)?;
        if json_mode() {
            println!("{}", serde_json::to_string_pretty(&t.docs)?);
        } else if t.docs.is_empty() {
            println!("{} has no docs", c.plan.label(&id));
        } else {
            for d in &t.docs {
                println!("{}", d);
            }
        }
        return Ok(());
    }
    let mut c = open_rw()?;
    let id = c.plan.resolve(&task)?;
    let t = c.plan.tasks.get_mut(&id).unwrap();
    let msg = if let Some(p) = path {
        let p = p.replace('\\', "/");
        if t.docs.contains(&p) {
            say("already linked");
            return Ok(());
        }
        t.docs.push(p.clone());
        format!("doc {} +{}", c.plan.number_of(&id), p)
    } else {
        let p = rm.unwrap().replace('\\', "/");
        let before = t.docs.len();
        t.docs.retain(|d| *d != p);
        if t.docs.len() == before {
            bail!("{} is not linked to {}", c.plan.label(&id), p);
        }
        format!("doc {} -{}", c.plan.number_of(&id), p)
    };
    let h = c.commit(Commit::new(EventKind::Set, msg.clone(), vec![id.clone()]))?;
    done(&c.store, &h, &msg);
    Ok(())
}

// ----- types ---------------------------------------------------------------

pub fn type_add(id: String, title: String, body: Option<String>, docs: Vec<String>) -> Result<()> {
    let body = read_text(body, false)?.unwrap_or_default();
    let mut c = open_rw()?;
    if c.plan.types.contains_key(&id) {
        bail!("type {} exists", id);
    }
    let docs = docs.into_iter().map(|d| d.replace('\\', "/")).collect();
    c.plan.types.insert(id.clone(), TaskType { id: id.clone(), title: title.clone(), body, items: vec![], docs });
    let h = c.commit(Commit::new(EventKind::TypeAdded, format!("type {}: {}", id, title), vec![]))?;
    done(&c.store, &h, &format!("added type {} ({})", id, title));
    Ok(())
}

fn parse_item_req(s: &str) -> Result<ItemReq> {
    let (blocking, s) = match s.strip_prefix('~') {
        Some(rest) => (false, rest),
        None => (true, s),
    };
    // `item:<id>: reason` has two colons; the first belongs to the ref.
    let (prefix, rest) = match s.strip_prefix("item:") {
        Some(rest) => ("item:", rest),
        None => ("", s),
    };
    let (on, reason) = rest.split_once(':').ok_or_else(|| anyhow!("`--req` wants `<core|item:<id>|task>: reason`, got `{}`", s))?;
    Ok(ItemReq { on: format!("{}{}", prefix, on.trim()), reason: reason.trim().to_string(), blocking })
}

pub(crate) fn instances_of(plan: &Plan, ty: &str, item: &str) -> Vec<TaskId> {
    let mut v: Vec<TaskId> = plan
        .tasks
        .values()
        .filter(|t| matches!(&t.from_item, Some((ty2, it)) if ty2 == ty && it == item))
        .map(|t| t.id.clone())
        .collect();
    v.sort();
    v
}

pub(crate) fn carriers(plan: &Plan, ty: &str) -> Vec<TaskId> {
    let mut v: Vec<TaskId> = plan.tasks.values().filter(|t| t.types.iter().any(|x| x == ty)).map(|t| t.id.clone()).collect();
    v.sort_by_key(|t| plan.tasks[t].seq);
    v
}

pub fn type_item(ty: String, id: String, title: String, body: Option<String>, pending: Option<String>, reqs: Vec<String>, when: Option<String>, docs: Vec<String>) -> Result<()> {
    let body = read_text(body, false)?.unwrap_or_default();
    let reqs: Vec<ItemReq> = reqs.iter().map(|s| parse_item_req(s)).collect::<Result<_>>()?;
    let mut c = open_rw()?;
    if let Some(p) = &pending {
        c.plan.resolve(p)?;
    }
    let t = c.plan.types.get_mut(&ty).ok_or_else(|| anyhow!("no type {}", ty))?;
    if t.items.iter().any(|i| i.id == id) {
        bail!("type {} already has item {}", ty, id);
    }
    let docs = docs.into_iter().map(|d| d.replace('\\', "/")).collect();
    t.items.push(TypeItem { id: id.clone(), title: title.clone(), body, pending, reqs, when, docs });
    let affected = carriers(&c.plan, &ty);
    let h = c.commit(Commit::new(EventKind::TypeEdited, format!("type {} item {}: {}", ty, id, title), affected.clone()))?;
    done(&c.store, &h, &format!("added item {} to type {}", id, ty));
    if !affected.is_empty() {
        say(&format!("{} task(s) carry this type and now owe this item; review each and `tsk amend` it:", affected.len()));
        for a in affected {
            say(&format!("  {}", c.plan.label(&a)));
        }
    }
    Ok(())
}

pub fn type_edit_item(ty: String, item: String, title: Option<String>, body: Option<String>) -> Result<()> {
    let body = read_text(body, false)?;
    let mut c = open_rw()?;
    let t = c.plan.types.get_mut(&ty).ok_or_else(|| anyhow!("no type {}", ty))?;
    let it = t.items.iter_mut().find(|i| i.id == item).ok_or_else(|| anyhow!("type {} has no item {}", ty, item))?;
    if let Some(ti) = title {
        it.title = ti;
    }
    if let Some(b) = body {
        it.body = b;
    }
    let inst = instances_of(&c.plan, &ty, &item);
    let h = c.commit(Commit::new(EventKind::TypeEdited, format!("type {} edit item {}", ty, item), inst.clone()))?;
    done(&c.store, &h, &format!("edited item {} of type {}", item, ty));
    if !inst.is_empty() {
        say(&format!("{} instance(s) carry this item's scope; review each, `tsk edit` it, `tsk uncomplete` it if it must reopen:", inst.len()));
        for i in inst {
            say(&format!("  {} {}", c.derived.status(&i).glyph(), c.plan.label(&i)));
        }
    }
    Ok(())
}

pub fn type_show(ty: String, json: bool) -> Result<()> {
    let c = open()?;
    let t = c.plan.types.get(&ty).ok_or_else(|| anyhow!("no type {}", ty))?;
    let carried = carriers(&c.plan, &ty);
    if json {
        println!("{}", serde_json::to_string_pretty(&json!({ "type": t, "carriers": carried }))?);
        return Ok(());
    }
    println!("type {}: {}", t.id, t.title);
    if !t.body.is_empty() {
        println!("{}", t.body);
    }
    if !t.docs.is_empty() {
        println!("docs: {}", t.docs.join(", "));
    }
    println!("items:");
    for i in &t.items {
        let p = i.pending.as_ref().map(|p| format!(" (pending {})", p)).unwrap_or_default();
        println!("  {}: {}{}", i.id, i.title, p);
        if let Some(w) = &i.when {
            println!("      when {}", w);
        }
        if !i.docs.is_empty() {
            println!("      docs {}", i.docs.join(", "));
        }
        for r in &i.reqs {
            println!("      {} {}: {}", if r.blocking { "after" } else { "needs" }, r.on, r.reason);
        }
    }
    println!("carried by {} task(s):", carried.len());
    for cid in carried {
        println!("  {} {}", c.derived.status(&cid).glyph(), c.plan.label(&cid));
    }
    Ok(())
}

pub fn type_list(json: bool) -> Result<()> {
    let c = open()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&c.plan.types.values().collect::<Vec<_>>())?);
        return Ok(());
    }
    for t in c.plan.types.values() {
        println!("{}  {}  ({} items, {} carriers)", t.id, t.title, t.items.len(), carriers(&c.plan, &t.id).len());
    }
    Ok(())
}

pub fn apply(ty: String, task: String) -> Result<()> {
    let mut c = open_rw()?;
    let id = c.plan.resolve(&task)?;
    if c.plan.tasks[&id].ongoing {
        bail!("{} is ongoing; a type's items are finite scope: apply it to a child of {}", c.plan.label(&id), c.plan.number_of(&id));
    }
    check_leaf_open(&c, &id, "apply to")?;
    let t = c.plan.types.get(&ty).cloned().ok_or_else(|| anyhow!("no type {}", ty))?;
    if t.items.is_empty() {
        bail!("type {} has no items yet", ty);
    }
    let host = c.plan.task(&id)?.clone();
    if host.types.contains(&ty) {
        bail!("{} already carries type {}", c.plan.label(&id), ty);
    }
    let mut subjects = vec![id.clone()];
    // core child: the task's own scope
    let core = new_task(&mut c.plan, &format!("{} (core)", host.title), Some(id.clone()), host.body.clone());
    c.plan.tasks.get_mut(&core).unwrap().core_of = Some(id.clone());
    subjects.push(core.clone());
    // one child per item
    let mut item_ids: Vec<(String, TaskId)> = Vec::new();
    for it in &t.items {
        let mut body = it.body.clone();
        if let Some(w) = &it.when {
            body = format!("When: {}{}{}", w, if body.is_empty() { "" } else { "\n\n" }, body);
        }
        let cid = new_task(&mut c.plan, &it.title, Some(id.clone()), body);
        let child = c.plan.tasks.get_mut(&cid).unwrap();
        child.from_item = Some((ty.clone(), it.id.clone()));
        child.docs = t.docs.iter().chain(it.docs.iter()).cloned().collect();
        subjects.push(cid.clone());
        item_ids.push((it.id.clone(), cid));
    }
    // requirements declared on the items
    for it in &t.items {
        let cid = item_ids.iter().find(|(i, _)| i == &it.id).unwrap().1.clone();
        for r in &it.reqs {
            let on = if r.on == "core" {
                core.clone()
            } else if let Some(i) = r.on.strip_prefix("item:") {
                item_ids.iter().find(|(x, _)| x == i).map(|(_, c)| c.clone()).ok_or_else(|| anyhow!("item {} refers to unknown item {}", it.id, i))?
            } else {
                c.plan.resolve(&r.on)?
            };
            add_req(&mut c, Requirer::Task(cid.clone()), on, r.reason.clone(), r.blocking, false, Some((ty.clone(), it.id.clone())));
        }
        if let Some(p) = &it.pending {
            let on = c.plan.resolve(p)?;
            add_req(&mut c, Requirer::Task(cid.clone()), on, format!("tentative: {} exists only if {} says so", it.title, p), true, true, Some((ty.clone(), it.id.clone())));
        }
    }
    // Existing requirements on the host stay on it: every child inherits them.
    let h = c.plan.tasks.get_mut(&id).unwrap();
    h.types.push(ty.clone());
    let mut narrative = format!("Carries type {} ({}). The core child is the task's own scope; the other children are the type's items, which every task of this type owes.", ty, t.title);
    if !t.body.is_empty() {
        narrative.push_str("\n\n");
        narrative.push_str(&t.body);
    }
    h.narrative = Some(narrative);
    let n = subjects.len() - 1;
    let hash = c.commit(Commit::new(EventKind::Applied, format!("apply {} to {}", ty, c.plan.number_of(&id)), subjects))?;
    done(&c.store, &hash, &format!("applied {} to {}: {} children", ty, c.plan.label(&id), n));
    for ch in c.plan.children(&id) {
        say(&format!("  {}", c.plan.label(&ch.id)));
    }
    Ok(())
}

// ----- ongoing scopes and dates --------------------------------------------

/// Declare a task ongoing (its last child is not yet known) with the text
/// of what belongs under it, or `--off` to take the declaration back.
pub fn ongoing(task: String, admits: Option<String>, off: bool) -> Result<()> {
    let mut c = open_rw()?;
    let id = c.plan.resolve(&task)?;
    let label = c.plan.label(&id);
    let t = c.plan.task(&id)?.clone();
    if c.derived.status(&id) == Status::Dropped {
        bail!("{} is dropped", label);
    }
    if c.derived.is_closed(&c.plan, &c.leaf, &id) {
        bail!("{} is closed; tsk reopen it first", label);
    }
    if off {
        if !t.ongoing {
            say("nothing changed");
            return Ok(());
        }
        c.plan.tasks.get_mut(&id).unwrap().ongoing = false;
        let h = c.commit(Commit::new(EventKind::Set, format!("ongoing {}: off", c.plan.number_of(&id)), vec![id.clone()]))?;
        done(&c.store, &h, &format!("{} is finite again; its narrative stays", label));
        return Ok(());
    }
    let admits = admits.map(|a| a.trim().to_string()).filter(|a| !a.is_empty());
    let Some(admits) = admits else {
        bail!("say what belongs under {}: tsk ongoing {} \"<what belongs under it>\", or --off", label, c.plan.number_of(&id));
    };
    if t.core_of.is_some() || t.from_item.is_some() {
        bail!("{} is the core or an item of a typed task; the host's cut defines it", label);
    }
    if !c.plan.is_parent(&id) && c.derived.status(&id) == Status::Complete {
        bail!("{} is complete; tsk uncomplete it first, or declare the task that holds its next piece ongoing", label);
    }
    let succ = successors_of(&c, &id);
    if !succ.is_empty() {
        bail!(
            "{} is a predecessor of {} ({}); an ongoing task never completes on its own: move those links to the piece they wait on, or to the finite task that means \"{} exists\"",
            label,
            succ.iter().map(|(l, _)| l.clone()).collect::<Vec<_>>().join("; "),
            succ.iter().map(|(_, r)| r.clone()).collect::<Vec<_>>().join(", "),
            t.title
        );
    }
    let was = c.derived.status(&id);
    let had_narrative = c.plan.is_parent(&id) && t.narrative.as_deref().map(|n| !n.trim().is_empty()).unwrap_or(false) && !t.ongoing;
    {
        let m = c.plan.tasks.get_mut(&id).unwrap();
        m.ongoing = true;
        m.narrative = Some(admits.clone());
    }
    let h = c.commit(Commit::new(EventKind::Set, format!("ongoing {}: {}", c.plan.number_of(&id), admits), vec![id.clone()]))?;
    done(&c.store, &h, &format!("{} is ongoing: complete only when closed", label));
    if had_narrative {
        say("  the narrative is now the admission test; the old one is in history");
    }
    if was == Status::Complete {
        say("  it was complete; it is in progress now, since its scope is open again");
    }
    Ok(())
}

/// Close an ongoing task: no more scope belongs under it. Refused while
/// anything under it is open.
pub fn close(task: String, reason: String) -> Result<()> {
    let c = open_rw()?;
    let id = c.plan.resolve(&task)?;
    let label = c.plan.label(&id);
    if !c.plan.tasks[&id].ongoing {
        bail!("{} is not ongoing; complete its leaves, or tsk ongoing it first", label);
    }
    if c.derived.status(&id) == Status::Dropped {
        bail!("{} is dropped", label);
    }
    if c.derived.is_closed(&c.plan, &c.leaf, &id) {
        bail!("{} is already closed", label);
    }
    let open = c.derived.open_under(&c.plan, &id);
    if !open.is_empty() {
        let mut names: Vec<String> = open.iter().take(5).map(|t| c.plan.label(t)).collect();
        if open.len() > 5 {
            names.push("...".into());
        }
        bail!("{} open under {}: complete or drop them first ({})", open.len(), label, names.join(", "));
    }
    let h = c.commit(Commit::new(EventKind::Closed, format!("close {}: {}", c.plan.number_of(&id), reason), vec![id.clone()]))?;
    done(&c.store, &h, &format!("closed {}: {}", label, reason));
    say_newly_ready(&c, &h);
    Ok(())
}

/// Reopen a closed ongoing task.
pub fn reopen(task: String) -> Result<()> {
    let c = open_rw()?;
    let id = c.plan.resolve(&task)?;
    let label = c.plan.label(&id);
    if !c.plan.tasks[&id].ongoing {
        bail!("{} is not ongoing; tsk uncomplete reopens a finite task", label);
    }
    if !c.derived.is_closed(&c.plan, &c.leaf, &id) {
        bail!("{} is not closed", label);
    }
    let h = c.commit(Commit::new(EventKind::Reopened, format!("reopen {}", c.plan.number_of(&id)), vec![id.clone()]))?;
    done(&c.store, &h, &format!("reopened {}: scope belongs under it again", label));
    Ok(())
}

/// Dates on a task: not workable before `--after` (inherited by
/// everything under it), and when it is `--due`. One `Set` event; the
/// reason, when given, records what the date stands for.
pub fn when(task: String, after: Option<String>, due: Option<String>, clear: Vec<String>, why: Option<String>) -> Result<()> {
    let after = parse_after(after)?;
    for k in &clear {
        if k != "after" && k != "due" {
            bail!("--clear takes `after` or `due`, not `{}`", k);
        }
    }
    let mut c = open_rw()?;
    let id = c.plan.resolve(&task)?;
    if c.derived.status(&id) == Status::Dropped {
        bail!("{} is dropped", c.plan.label(&id));
    }
    let mut t = c.plan.task(&id)?.clone();
    let mut changed: Vec<String> = Vec::new();
    if let Some(a) = after {
        if t.after.as_deref() != Some(a.as_str()) {
            changed.push(format!("after={}", a));
            t.after = Some(a);
        }
    }
    if let Some(d) = due {
        let d = crate::clock::normalize_due(&d);
        if t.due.as_deref() != Some(d.as_str()) {
            changed.push(format!("due={}", d));
            t.due = Some(d);
        }
    }
    for k in &clear {
        let was = if k == "after" { t.after.take() } else { t.due.take() };
        if was.is_some() {
            changed.push(format!("-{}", k));
        }
    }
    if changed.is_empty() {
        say("nothing changed");
        return Ok(());
    }
    c.plan.tasks.insert(id.clone(), t);
    let why = why.map(|w| w.trim().to_string()).filter(|w| !w.is_empty()).map(|w| format!(": {}", w)).unwrap_or_default();
    let h = c.commit(Commit::new(EventKind::Set, format!("when {} {}{}", c.plan.number_of(&id), changed.join(" "), why), vec![id.clone()]))?;
    done(&c.store, &h, &format!("when {}: {}", c.plan.label(&id), changed.join(" ")));
    if let Some((_, d)) = c.derived.not_before(&c.plan, &id) {
        if c.plan.tasks[&id].after.as_deref() == Some(d.as_str()) {
            say(&format!("  not before {}: the first tsk ready on or after it lists what this holds back", d));
        }
    }
    Ok(())
}

/// After a completion or a close, what became ready, and which ongoing
/// ancestor now has nothing open under it.
fn say_newly_ready(c: &Ctx, h: &Hash) {
    let mut leaf = c.leaf.clone();
    let Ok(ev) = c.store.read_object::<Event>(ObjectType::Event, h) else { return };
    crate::state::apply_event(&mut leaf, &ev);
    let d = Derived::compute(&c.plan, &leaf, c.derived.now);
    let newly: Vec<TaskId> = d.ready(&c.plan).into_iter().filter(|t| !c.derived.is_ready(&c.plan, t)).collect();
    if !newly.is_empty() {
        say("now ready:");
        for t in newly {
            say(&format!("  {}", c.plan.label(&t)));
        }
    }
    if ev.kind != EventKind::Completed {
        return;
    }
    let mut nudged: Vec<TaskId> = Vec::new();
    for s in &ev.subjects {
        for a in c.plan.ancestors_and_self(s) {
            if a.id == *s || !a.ongoing || nudged.contains(&a.id) || d.is_closed(&c.plan, &leaf, &a.id) {
                continue;
            }
            if d.open_under(&c.plan, &a.id).is_empty() {
                nudged.push(a.id.clone());
                say(&format!("{} is ongoing and nothing is open under it now: add the next piece (tsk amend {} ... --after ...), or tsk close it", c.plan.label(&a.id), c.plan.number_of(&a.id)));
            }
        }
    }
}

// ----- state changes ------------------------------------------------------

pub fn spike(task: String) -> Result<()> {
    let c = open_rw()?;
    let id = c.plan.resolve(&task)?;
    check_not_ongoing(&c, &id)?;
    check_leaf_open(&c, &id, "spike")?;
    if c.derived.status(&id) == Status::Spike {
        bail!("{} is already a spike", c.plan.label(&id));
    }
    let h = c.commit(Commit::new(EventKind::FlaggedAsSpike, format!("spike {}", c.plan.number_of(&id)), vec![id.clone()]))?;
    done(&c.store, &h, &format!("flagged {} as a spike: prove it, then break it down", c.plan.label(&id)));
    Ok(())
}

pub fn confirm(task: String) -> Result<()> {
    let mut c = open_rw()?;
    let id = c.plan.resolve(&task)?;
    let mut n = 0;
    for r in c.plan.reqs.values_mut() {
        if r.conditional && matches!(&r.from, Requirer::Task(t) if *t == id) {
            r.conditional = false;
            n += 1;
        }
    }
    if n == 0 {
        bail!("{} is not tentative", c.plan.label(&id));
    }
    let h = c.commit(Commit::new(EventKind::Confirmed, format!("confirm {}", c.plan.number_of(&id)), vec![id.clone()]))?;
    done(&c.store, &h, &format!("confirmed {}: it is real work now", c.plan.label(&id)));
    Ok(())
}

/// The loop-group rule shared by start and complete. Returns the blocks
/// that the group does not cover.
fn uncovered(c: &Ctx, id: &str, group: &BTreeSet<TaskId>) -> Vec<String> {
    let mut out = Vec::new();
    for b in c.derived.blocks(&c.plan, id) {
        match b {
            Block::Loop(i) => {
                let lp = &c.derived.loops[i];
                let missing: Vec<String> = lp.tasks.iter().filter(|t| !group.contains(*t)).map(|t| c.plan.label(t)).collect();
                if !missing.is_empty() {
                    out.push(format!("in a loop with: {}", missing.join("; ")));
                }
            }
            Block::Tentative(r, met) => {
                let req = &c.plan.reqs[&r];
                if met {
                    out.push(format!("tentative, and {} is complete: `tsk confirm` or `tsk drop` it ({})", c.plan.label(&req.on), r));
                } else {
                    out.push(format!("tentative until {} is complete ({})", c.plan.label(&req.on), r));
                }
            }
            Block::Waiting(p, r, _) => {
                // Covered when every open task under the predecessor is in the group.
                let open_under: Vec<TaskId> = c
                    .plan
                    .subtree(&p)
                    .into_iter()
                    .filter(|t| !c.plan.is_parent(t) && !matches!(c.derived.status(t), Status::Complete | Status::Dropped))
                    .collect();
                if open_under.iter().all(|t| group.contains(t)) {
                    continue;
                }
                let req = &c.plan.reqs[&r];
                out.push(format!("waits on {} ({}: {})", c.plan.label(&p), r, req.reason));
            }
            // A date is not covered by a group; `--force` records an early start.
            Block::NotBefore(t, d) => {
                if t == id {
                    out.push(format!("not before {}", d));
                } else {
                    out.push(format!("not before {} (after, on {})", d, c.plan.number_of(&t)));
                }
            }
        }
    }
    out
}

fn transition(tasks: Vec<String>, force: bool, kind: EventKind) -> Result<()> {
    if tasks.is_empty() {
        bail!("name at least one task");
    }
    let c = open_rw()?;
    let ids: Vec<TaskId> = tasks.iter().map(|t| c.plan.resolve(t)).collect::<Result<_>>()?;
    let group: BTreeSet<TaskId> = ids.iter().cloned().collect();
    let completing = kind == EventKind::Completed;
    let verb = if completing { "complete" } else { "start" };
    let mut problems = Vec::new();
    for id in &ids {
        check_not_ongoing(&c, id)?;
        if c.plan.is_parent(id) {
            bail!("{} is a parent: its status comes from its children; {} them instead", c.plan.label(id), verb);
        }
        match (c.derived.status(id), completing) {
            (Status::Complete, _) => bail!("{} is already complete", c.plan.label(id)),
            (Status::Dropped, _) => bail!("{} is dropped", c.plan.label(id)),
            (Status::InProgress, false) => bail!("{} is already in progress", c.plan.label(id)),
            _ => {}
        }
        for u in uncovered(&c, id, &group) {
            problems.push(format!("{}: {}", c.plan.label(id), u));
        }
    }
    if !problems.is_empty() && !force {
        let mut msg = format!("refusing to {}:\n", verb);
        for p in &problems {
            msg.push_str(&format!("  {}\n", p));
        }
        msg.push_str("name every task in the loop in one command, resolve the dependency, or pass --force (recorded; see `tsk forced`)");
        bail!("{}", msg.trim_end());
    }
    let mut cm = Commit::new(kind, format!("{} {}", verb, ids.iter().map(|i| c.plan.number_of(i)).collect::<Vec<_>>().join(" ")), ids.clone());
    if ids.len() > 1 {
        cm.group = ids.clone();
    }
    cm.forced = !problems.is_empty();
    let h = c.commit(cm)?;
    let labels: Vec<String> = ids.iter().map(|i| c.plan.label(i)).collect();
    let forced = if problems.is_empty() { String::new() } else { format!("{} ", crate::color::red("FORCED")) };
    done(&c.store, &h, &format!("{}{}: {}", forced, kind.name(), labels.join("; ")));
    if completing {
        say_newly_ready(&c, &h);
    }
    Ok(())
}

pub fn start(tasks: Vec<String>, force: bool) -> Result<()> {
    transition(tasks, force, EventKind::Started)
}

pub fn complete(tasks: Vec<String>, force: bool) -> Result<()> {
    transition(tasks, force, EventKind::Completed)
}

pub fn pause(task: String) -> Result<()> {
    let c = open_rw()?;
    let id = c.plan.resolve(&task)?;
    check_not_ongoing(&c, &id)?;
    if c.derived.status(&id) != Status::InProgress {
        bail!("{} is not in progress", c.plan.label(&id));
    }
    let h = c.commit(Commit::new(EventKind::Paused, format!("pause {}", c.plan.number_of(&id)), vec![id.clone()]))?;
    done(&c.store, &h, &format!("paused {}", c.plan.label(&id)));
    Ok(())
}

pub fn uncomplete(task: String) -> Result<()> {
    let c = open_rw()?;
    let id = c.plan.resolve(&task)?;
    if c.plan.tasks[&id].ongoing {
        bail!("{} is ongoing; tsk reopen it", c.plan.label(&id));
    }
    if c.plan.is_parent(&id) {
        bail!("{} is a parent; uncomplete one of its children", c.plan.label(&id));
    }
    if c.derived.status(&id) != Status::Complete {
        bail!("{} is not complete", c.plan.label(&id));
    }
    let h = c.commit(Commit::new(EventKind::Uncompleted, format!("uncomplete {}", c.plan.number_of(&id)), vec![id.clone()]))?;
    done(&c.store, &h, &format!("reopened {}", c.plan.label(&id)));
    Ok(())
}

pub fn drop(task: String, reason: String) -> Result<()> {
    let c = open_rw()?;
    let id = c.plan.resolve(&task)?;
    if c.derived.status(&id) == Status::Dropped {
        bail!("{} is already dropped", c.plan.label(&id));
    }
    let h = c.commit(Commit::new(EventKind::Dropped, format!("drop {}: {}", c.plan.number_of(&id), reason), vec![id.clone()]))?;
    done(&c.store, &h, &format!("dropped {} and everything under it: {}", c.plan.label(&id), reason));
    Ok(())
}

pub fn config(key: Option<String>, value: Option<String>) -> Result<()> {
    let store = store_open()?;
    let mut cfg = store.read_config()?;
    match (key, value) {
        (None, _) => {
            for (k, v) in &cfg {
                println!("{} = {}", k, v);
            }
        }
        (Some(k), None) => match cfg.get(&k) {
            Some(v) => println!("{}", v),
            None => bail!("{} is not set", k),
        },
        (Some(k), Some(v)) => {
            cfg.insert(k, v);
            store.write_config(&cfg)?;
        }
    }
    Ok(())
}

pub fn reflog(limit: Option<usize>, json: bool) -> Result<()> {
    let store = store_open()?;
    let entries = store.reflog()?;
    let rows: Vec<_> = entries.iter().take(limit.unwrap_or(usize::MAX)).collect();
    if json {
        let v: Vec<_> = rows.iter().map(|e| json!({"old": e.old, "new": e.new, "time": e.time, "author": e.author, "what": e.what})).collect();
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("no moves recorded on this copy");
        return Ok(());
    }
    for e in rows {
        let was = e.old.as_deref().map(|o| format!("  (was {})", &o[..10])).unwrap_or_default();
        println!("{}  {}  {}{}", &e.new[..10], e.time, e.what, was);
    }
    Ok(())
}

/// Move HEAD to any event whose plan can still be read. The events after
/// it stay in the store, and the reflog remembers where HEAD was.
pub fn reset(event: String) -> Result<()> {
    let store = store_open()?;
    let _lock = store.lock()?;
    let h = store.resolve_hash(&event)?;
    let ev: Event = store.read_object(ObjectType::Event, &h).with_context(|| format!("{} is not an event", &h[..10]))?;
    let snap = store.read_snapshot(&ev.snapshot).with_context(|| format!("event {}", &h[..10]))?;
    let old = store.head()?;
    if old.as_deref() == Some(h.as_str()) {
        say(&format!("HEAD is already at {}", &h[..10]));
        return Ok(());
    }
    store.move_head(&h, &format!("reset to {}: {} {}", &h[..10], ev.kind.name(), ev.message))?;
    // Committing on top of the event we reset to is legitimate again, so
    // its claim (from the line we are leaving) is released.
    let _ = std::fs::remove_file(store.dir.join("claims").join(&h));
    let plan = Plan::load(&store, &snap)?;
    let history = store.history()?;
    store.rebuild_log(&history, &plan)?;
    if json_mode() {
        println!("{}", json!({"head": h, "was": old, "kind": ev.kind.name(), "message": ev.message, "time": ev.time}));
    } else {
        println!("HEAD is now at {}  {}  {}", &h[..10], ev.kind.name(), ev.message);
        if let Some(o) = old {
            println!("it was {}; `tsk reset {}` returns there (see `tsk reflog`)", &o[..10], &o[..10]);
        }
    }
    Ok(())
}

// ===== plumbing ============================================================

pub fn cat_object(hash: String) -> Result<()> {
    let store = store_open()?;
    let (ty, body) = store.read_raw(&hash)?;
    println!("{}", ty.name());
    println!("{}", body.trim_end());
    Ok(())
}

pub fn hash_object(file: Option<String>) -> Result<()> {
    let text = match file {
        Some(f) => std::fs::read_to_string(Path::new(&f))?,
        None => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            s
        }
    };
    println!("{}", Store::hash_bytes(text.as_bytes()));
    Ok(())
}

pub fn ls_snapshot(event: Option<String>) -> Result<()> {
    let store = store_open()?;
    let h = match event {
        Some(e) => store.resolve_hash(&e)?,
        None => store.head()?.ok_or_else(|| anyhow!("no HEAD"))?,
    };
    let ev: Event = store.read_object(ObjectType::Event, &h)?;
    let snap = store.read_snapshot(&ev.snapshot)?;
    println!("snapshot {}", ev.snapshot);
    for (id, h) in &snap.tasks {
        println!("task  {}  {}", h, id);
    }
    for (id, h) in &snap.reqs {
        println!("req   {}  {}", h, id);
    }
    for (id, h) in &snap.types {
        println!("type  {}  {}", h, id);
    }
    Ok(())
}

pub fn rev_list(limit: Option<usize>) -> Result<()> {
    let store = store_open()?;
    for (h, _) in store.history()?.iter().take(limit.unwrap_or(usize::MAX)) {
        println!("{}", h);
    }
    Ok(())
}
