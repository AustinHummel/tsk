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
    /// Per-leaf statuses HEAD implies (from the cache, or the history).
    pub leaf: std::collections::HashMap<TaskId, Status>,
    pub derived: Derived,
    /// The whole event history, read only by the commands that show it.
    history: OnceLock<Vec<(Hash, Event)>>,
    /// Held for the life of a writing command.
    _lock: Option<Lock>,
}

impl Ctx {
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
    match STORE.get().cloned().flatten() {
        Some(p) => Store::at(&p),
        None => Store::discover(&std::env::current_dir()?),
    }
}

fn load(store: Store, lock: Option<Lock>) -> Result<Ctx> {
    let (plan, leaf) = match store.head_event()? {
        None => (Plan::default(), std::collections::HashMap::new()),
        Some((h, ev)) => (store.load_plan(&ev.snapshot)?, store.leaf_statuses_at(&h)?),
    };
    let derived = Derived::compute(&plan, &leaf);
    Ok(Ctx { store, plan, leaf, derived, history: OnceLock::new(), _lock: lock })
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
        println!("{}  ({})", msg, &hash[..10]);
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
            priority: None,
            due: None,
            docs: vec![],
            fields: Fields::new(),
        },
    );
    id
}

// ===== porcelain ===========================================================

pub fn init() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let store = Store::init(&cwd)?;
    let plan = Plan::default();
    let h = commit::write(&store, &plan, Commit::new(EventKind::Init, "init", vec![]))?;
    done(&store, &h, &format!("initialized {}", store.dir.display()));
    Ok(())
}

pub fn add(title: String, body: Option<String>, by: Vec<String>, priority: Option<String>, due: Option<String>) -> Result<()> {
    let body = read_text(body, false)?.unwrap_or_default();
    let mut c = open_rw()?;
    let id = new_task(&mut c.plan, &title, None, body);
    {
        let t = c.plan.tasks.get_mut(&id).unwrap();
        t.priority = priority;
        t.due = due;
    }
    for b in by {
        let (p, r) = parse_by(&b)?;
        add_req(&mut c, Requirer::Person(p), id.clone(), r, false, false, None);
    }
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Created, format!("add {}", title), vec![id.clone()]))?;
    done(&c.store, &h, &format!("created {}", c.plan.label(&id)));
    Ok(())
}

fn check_leaf_open(c: &Ctx, id: &str, verb: &str) -> Result<()> {
    if c.plan.is_parent(id) {
        bail!("{} is already broken down; {} one of its children instead", c.plan.label(id), verb);
    }
    match c.derived.status(id) {
        Status::Complete => bail!("{} is complete; `tsk uncomplete` it first", c.plan.label(id)),
        Status::Dropped => bail!("{} is dropped", c.plan.label(id)),
        _ => Ok(()),
    }
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
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::BrokenDown, format!("breakdown {} into {} children", c.plan.number_of(&id), n), subjects))?;
    done(&c.store, &h, &format!("broke down {} into {} children", c.plan.label(&id), n));
    for cid in &child_ids {
        say(&format!("  {}", c.plan.label(cid)));
    }
    Ok(())
}

pub fn amend(parent: String, title: String, body: Option<String>, narrative: Option<String>) -> Result<()> {
    let narrative = narrative.ok_or_else(|| anyhow!("`--narrative` is required: say what scope the new child covers and why the parent gained it"))?;
    let body = read_text(body, false)?.unwrap_or_default();
    let mut c = open_rw()?;
    let pid = c.plan.resolve(&parent)?;
    if !c.plan.is_parent(&pid) {
        bail!("{} is not broken down; use `tsk breakdown`", c.plan.label(&pid));
    }
    let cid = new_task(&mut c.plan, &title, Some(pid.clone()), body);
    let p = c.plan.tasks.get_mut(&pid).unwrap();
    let mut n = p.narrative.clone().unwrap_or_default();
    if !n.is_empty() {
        n.push_str("\n\n");
    }
    n.push_str(&format!("Amended {}: {}", commit::now(), narrative));
    p.narrative = Some(n);
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Amended, format!("amend {} with {}", c.plan.number_of(&pid), title), vec![pid.clone(), cid.clone()]))?;
    done(&c.store, &h, &format!("added {} under {}", c.plan.label(&cid), c.plan.label(&pid)));
    Ok(())
}

pub fn edit(task: String, file: Option<String>, body: Option<String>, title: Option<String>, narrative: Option<String>) -> Result<()> {
    let mut new_body = match (file, body) {
        (Some(f), _) => read_text(Some(f), true)?,
        (None, Some(b)) => read_text(Some(b), false)?,
        (None, None) => None,
    };
    if new_body.is_none() && title.is_none() && narrative.is_none() {
        let c = open()?;
        let id = c.plan.resolve(&task)?;
        new_body = Some(edit_in_editor(&c.store, "CARD", &c.plan.task(&id)?.body)?);
    }
    let mut c = open_rw()?;
    let id = c.plan.resolve(&task)?;
    let mut t = c.plan.task(&id)?.clone();
    let mut changed = Vec::new();
    if let Some(n) = narrative {
        if !c.plan.is_parent(&id) {
            bail!("{} has no children; the narrative is set by breakdown", c.plan.label(&id));
        }
        t.narrative = Some(n);
        changed.push("narrative");
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
    if changed.is_empty() {
        say("nothing changed");
        return Ok(());
    }
    c.plan.tasks.insert(id.clone(), t);
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Edited, format!("edit {} ({})", c.plan.number_of(&id), changed.join(", ")), vec![id.clone()]))?;
    done(&c.store, &h, &format!("edited {}: {}", c.plan.label(&id), changed.join(", ")));
    Ok(())
}

fn add_req(c: &mut Ctx, from: Requirer, on: TaskId, reason: String, blocking: bool, conditional: bool, from_item: Option<(TypeId, String)>) -> ReqId {
    let rid = c.plan.new_req_id();
    c.plan.reqs.insert(rid.clone(), Requirement { id: rid.clone(), from, on, reason, blocking, conditional, created: commit::now(), from_item, fields: Fields::new() });
    rid
}

fn note_loops(c: &Ctx, before: usize) -> Result<()> {
    let d = Derived::compute(&c.plan, &c.leaf);
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
    let before = c.derived.loops.len();
    let rid = add_req(&mut c, Requirer::Task(s.clone()), p.clone(), reason.clone(), true, conditional, None);
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Linked, format!("link {} -> {}: {}", c.plan.number_of(&p), c.plan.number_of(&s), reason), vec![p.clone(), s.clone()]))?;
    if conditional {
        done(&c.store, &h, &format!("{}: {} is tentative until {} is complete", rid, c.plan.label(&s), c.plan.label(&p)));
    } else {
        done(&c.store, &h, &format!("{}: {} is predecessor of {}", rid, c.plan.label(&p), c.plan.label(&s)));
    }
    note_loops(&c, before)
}

pub fn require(from: String, on: String, reason: String) -> Result<()> {
    let mut c = open_rw()?;
    let f = c.plan.resolve(&from)?;
    let o = c.plan.resolve(&on)?;
    if f == o {
        bail!("a task cannot require something of itself");
    }
    let rid = add_req(&mut c, Requirer::Task(f.clone()), o.clone(), reason.clone(), false, false, None);
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Required, format!("require {} of {}: {}", c.plan.number_of(&f), c.plan.number_of(&o), reason), vec![f.clone(), o.clone()]))?;
    done(&c.store, &h, &format!("{}: {} requires of {}: {}", rid, c.plan.label(&f), c.plan.label(&o), reason));
    Ok(())
}

pub fn by(task: String, person: String, reason: String) -> Result<()> {
    let mut c = open_rw()?;
    let id = c.plan.resolve(&task)?;
    let rid = add_req(&mut c, Requirer::Person(person.clone()), id.clone(), reason.clone(), false, false, None);
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Required, format!("{} requires {}: {}", person, c.plan.number_of(&id), reason), vec![id.clone()]))?;
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
    let before = c.derived.loops.len();
    c.plan.reqs.insert(req.clone(), r.clone());
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Moved, format!("move {}", req), subjects))?;
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
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Unlinked, format!("unlink {}: {}", req, r.reason), subjects))?;
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
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Set, format!("set {} {}", label, changed.join(" ")), subjects))?;
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
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Set, msg.clone(), vec![id.clone()]))?;
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
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::TypeAdded, format!("type {}: {}", id, title), vec![]))?;
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
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::TypeEdited, format!("type {} item {}: {}", ty, id, title), affected.clone()))?;
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
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::TypeEdited, format!("type {} edit item {}", ty, item), inst.clone()))?;
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
    let hash = commit::write(&c.store, &c.plan, Commit::new(EventKind::Applied, format!("apply {} to {}", ty, c.plan.number_of(&id)), subjects))?;
    done(&c.store, &hash, &format!("applied {} to {}: {} children", ty, c.plan.label(&id), n));
    for ch in c.plan.children(&id) {
        say(&format!("  {}", c.plan.label(&ch.id)));
    }
    Ok(())
}

// ----- state changes ------------------------------------------------------

pub fn spike(task: String) -> Result<()> {
    let c = open_rw()?;
    let id = c.plan.resolve(&task)?;
    check_leaf_open(&c, &id, "spike")?;
    if c.derived.status(&id) == Status::Spike {
        bail!("{} is already a spike", c.plan.label(&id));
    }
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::FlaggedAsSpike, format!("spike {}", c.plan.number_of(&id)), vec![id.clone()]))?;
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
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Confirmed, format!("confirm {}", c.plan.number_of(&id)), vec![id.clone()]))?;
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
    let h = commit::write(&c.store, &c.plan, cm)?;
    let labels: Vec<String> = ids.iter().map(|i| c.plan.label(i)).collect();
    done(&c.store, &h, &format!("{}{}: {}", if !problems.is_empty() { "FORCED " } else { "" }, kind.name(), labels.join("; ")));
    if completing {
        // Say what just became ready.
        let mut leaf = c.leaf.clone();
        if let Ok(ev) = c.store.read_object::<Event>(ObjectType::Event, &h) {
            crate::state::apply_event(&mut leaf, &ev);
        }
        let d = Derived::compute(&c.plan, &leaf);
        let newly: Vec<TaskId> = d.ready(&c.plan).into_iter().filter(|t| !c.derived.is_ready(&c.plan, t)).collect();
        if !newly.is_empty() {
            say("now ready:");
            for t in newly {
                say(&format!("  {}", c.plan.label(&t)));
            }
        }
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
    if c.derived.status(&id) != Status::InProgress {
        bail!("{} is not in progress", c.plan.label(&id));
    }
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Paused, format!("pause {}", c.plan.number_of(&id)), vec![id.clone()]))?;
    done(&c.store, &h, &format!("paused {}", c.plan.label(&id)));
    Ok(())
}

pub fn uncomplete(task: String) -> Result<()> {
    let c = open_rw()?;
    let id = c.plan.resolve(&task)?;
    if c.plan.is_parent(&id) {
        bail!("{} is a parent; uncomplete one of its children", c.plan.label(&id));
    }
    if c.derived.status(&id) != Status::Complete {
        bail!("{} is not complete", c.plan.label(&id));
    }
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Uncompleted, format!("uncomplete {}", c.plan.number_of(&id)), vec![id.clone()]))?;
    done(&c.store, &h, &format!("reopened {}", c.plan.label(&id)));
    Ok(())
}

pub fn drop(task: String, reason: String) -> Result<()> {
    let c = open_rw()?;
    let id = c.plan.resolve(&task)?;
    if c.derived.status(&id) == Status::Dropped {
        bail!("{} is already dropped", c.plan.label(&id));
    }
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Dropped, format!("drop {}: {}", c.plan.number_of(&id), reason), vec![id.clone()]))?;
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
