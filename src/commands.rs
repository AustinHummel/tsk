//! Porcelain and plumbing. Each porcelain command loads the plan, changes
//! it in memory, and writes exactly one event.

use crate::breakdown;
use crate::commit::{self, Commit};
use crate::model::*;
use crate::plan::Plan;
use crate::state::{Block, Derived, Status};
use crate::store::Store;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::json;
use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;

struct Ctx {
    store: Store,
    plan: Plan,
    history: Vec<(Hash, Event)>,
    derived: Derived,
}

fn open() -> Result<Ctx> {
    let store = Store::discover(&std::env::current_dir()?)?;
    let snap = store.head_snapshot()?;
    let plan = Plan::load(&store, &snap)?;
    let history = store.history()?;
    let derived = Derived::compute(&plan, &history);
    Ok(Ctx { store, plan, history, derived })
}

fn read_text(arg: Option<String>, from_file: bool) -> Result<Option<String>> {
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
        .read_config()
        .ok()
        .and_then(|c| c.get("editor").cloned())
        .or_else(|| std::env::var("TSK_EDITOR").ok())
        .or_else(|| std::env::var("VISUAL").ok())
        .or_else(|| std::env::var("EDITOR").ok())
        .unwrap_or_else(|| if cfg!(windows) { "notepad".to_string() } else { "vi".to_string() })
}

/// Open `initial` in the editor and return what was saved.
fn edit_in_editor(store: &Store, name: &str, initial: &str) -> Result<String> {
    let path = store.dir.join(format!("{}.EDIT", name));
    std::fs::write(&path, initial)?;
    let cmd = editor_cmd(store);
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

fn req_desc(plan: &Plan, r: &Requirement) -> String {
    let kind = if r.conditional {
        "conditional"
    } else if r.blocking {
        "blocking"
    } else {
        "technical"
    };
    format!("{} on [{}] from [{}]: {}", kind, plan.label(&r.on), plan.requirer_label(&r.from), r.reason)
}

fn done(hash: &Hash, msg: &str) {
    println!("{}  ({})", msg, &hash[..10]);
}

// ===== porcelain ===========================================================

pub fn init() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let store = Store::init(&cwd)?;
    let plan = Plan::default();
    let h = commit::write(&store, &plan, Commit::new(EventKind::Init, "init", vec![]))?;
    done(&h, &format!("initialized {}", store.dir.display()));
    Ok(())
}

pub fn add(title: String, body: Option<String>, by: Vec<String>, priority: Option<String>, due: Option<String>) -> Result<()> {
    let mut c = open()?;
    let id = c.plan.new_task_id();
    let seq = c.plan.next_task;
    let body = read_text(body, false)?.unwrap_or_default();
    c.plan.tasks.insert(
        id.clone(),
        Task { id: id.clone(), title: title.clone(), parent: None, seq, body, types: vec![], from_item: None, core_of: None, narrative: None, priority, due },
    );
    for b in by {
        let (p, r) = parse_by(&b)?;
        let rid = c.plan.new_req_id();
        c.plan.reqs.insert(rid.clone(), Requirement { id: rid, from: Requirer::Person(p), on: id.clone(), reason: r, blocking: false, conditional: false, created: commit::now(), from_item: None });
    }
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Created, format!("add {}", title), vec![id.clone()]))?;
    done(&h, &format!("created {}", c.plan.label(&id)));
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

pub fn breakdown(task: String, file: Option<String>, template: bool) -> Result<()> {
    let mut c = open()?;
    let id = c.plan.resolve(&task)?;
    check_leaf_open(&c, &id, "break down")?;
    let parent = c.plan.task(&id)?.clone();

    // Every requirement touching the parent must be dispositioned.
    let mut to_place: Vec<(String, String)> = Vec::new();
    for r in c.plan.reqs_on(&id) {
        if matches!(r.from, Requirer::Task(_)) {
            to_place.push((r.id.clone(), req_desc(&c.plan, r)));
        }
    }
    for r in c.plan.reqs_from(&id) {
        to_place.push((r.id.clone(), req_desc(&c.plan, r)));
    }
    let tmpl = breakdown::template(&c.plan.label(&id), &parent.body, &to_place);
    if template {
        print!("{}", tmpl);
        return Ok(());
    }
    let text = match read_text(file, true)? {
        Some(t) => t,
        None => edit_in_editor(&c.store, "BREAKDOWN", &tmpl)?,
    };
    let bd = breakdown::parse(&text)?;
    if bd.narrative.is_empty() {
        bail!("the breakdown needs a `= narrative`: how the children together fulfill {}", c.plan.label(&id));
    }
    let missing: Vec<&str> = to_place.iter().filter(|(r, _)| !bd.dispositions.contains_key(r)).map(|(r, _)| r.as_str()).collect();
    if !missing.is_empty() {
        bail!("every requirement on the parent must go to a child or `all`; missing: {}", missing.join(", "));
    }
    for r in bd.dispositions.keys() {
        if !to_place.iter().any(|(x, _)| x == r) {
            bail!("requirement {} does not touch {}", r, c.plan.label(&id));
        }
    }

    let mut subjects = vec![id.clone()];
    let mut child_ids = Vec::new();
    for (title, body) in &bd.children {
        let cid = c.plan.new_task_id();
        let seq = c.plan.next_task;
        c.plan.tasks.insert(cid.clone(), Task { id: cid.clone(), title: title.clone(), parent: Some(id.clone()), seq, body: body.clone(), types: vec![], from_item: None, core_of: None, narrative: None, priority: None, due: None });
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
    done(&h, &format!("broke down {} into {} children", c.plan.label(&id), n));
    for cid in &child_ids {
        println!("  {}", c.plan.label(cid));
    }
    Ok(())
}

pub fn amend(parent: String, title: String, body: Option<String>, narrative: Option<String>) -> Result<()> {
    let mut c = open()?;
    let pid = c.plan.resolve(&parent)?;
    if !c.plan.is_parent(&pid) {
        bail!("{} is not broken down; use `tsk breakdown`", c.plan.label(&pid));
    }
    let narrative = narrative.ok_or_else(|| anyhow!("`--narrative` is required: say what scope the new child covers and why the parent gained it"))?;
    let body = read_text(body, false)?.unwrap_or_default();
    let cid = c.plan.new_task_id();
    let seq = c.plan.next_task;
    c.plan.tasks.insert(cid.clone(), Task { id: cid.clone(), title: title.clone(), parent: Some(pid.clone()), seq, body, types: vec![], from_item: None, core_of: None, narrative: None, priority: None, due: None });
    let p = c.plan.tasks.get_mut(&pid).unwrap();
    let mut n = p.narrative.clone().unwrap_or_default();
    if !n.is_empty() {
        n.push_str("\n\n");
    }
    n.push_str(&format!("Amended {}: {}", commit::now(), narrative));
    p.narrative = Some(n);
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Amended, format!("amend {} with {}", c.plan.number_of(&pid), title), vec![pid.clone(), cid.clone()]))?;
    done(&h, &format!("added {} under {}", c.plan.label(&cid), c.plan.label(&pid)));
    Ok(())
}

pub fn edit(task: String, file: Option<String>, body: Option<String>, title: Option<String>, narrative: Option<String>) -> Result<()> {
    let mut c = open()?;
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
    let new_body = match (file, body) {
        (Some(f), _) => read_text(Some(f), true)?,
        (None, Some(b)) => read_text(Some(b), false)?,
        (None, None) if changed.is_empty() => Some(edit_in_editor(&c.store, "CARD", &t.body)?),
        _ => None,
    };
    if let Some(b) = new_body {
        if b.trim_end() != t.body.trim_end() {
            t.body = b.trim_end().to_string();
            changed.push("body");
        }
    }
    if changed.is_empty() {
        println!("nothing changed");
        return Ok(());
    }
    c.plan.tasks.insert(id.clone(), t);
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Edited, format!("edit {} ({})", c.plan.number_of(&id), changed.join(", ")), vec![id.clone()]))?;
    done(&h, &format!("edited {}: {}", c.plan.label(&id), changed.join(", ")));
    Ok(())
}

fn add_req(c: &mut Ctx, from: Requirer, on: TaskId, reason: String, blocking: bool, conditional: bool, from_item: Option<(TypeId, String)>) -> ReqId {
    let rid = c.plan.new_req_id();
    c.plan.reqs.insert(rid.clone(), Requirement { id: rid.clone(), from, on, reason, blocking, conditional, created: commit::now(), from_item });
    rid
}

fn note_loops(store: &Store, plan: &Plan, before: usize) -> Result<()> {
    let history = store.history()?;
    let d = Derived::compute(plan, &history);
    if d.loops.len() > before {
        println!("note: this forms a dependency loop; see `tsk loops`");
    }
    Ok(())
}

pub fn link(pred: String, succ: String, reason: String, conditional: bool) -> Result<()> {
    let mut c = open()?;
    let p = c.plan.resolve(&pred)?;
    let s = c.plan.resolve(&succ)?;
    if p == s {
        bail!("a task cannot be its own predecessor");
    }
    let before = c.derived.loops.len();
    let rid = add_req(&mut c, Requirer::Task(s.clone()), p.clone(), reason.clone(), true, conditional, None);
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Linked, format!("link {} -> {}: {}", c.plan.number_of(&p), c.plan.number_of(&s), reason), vec![p.clone(), s.clone()]))?;
    if conditional {
        done(&h, &format!("{}: {} is tentative until {} is complete", rid, c.plan.label(&s), c.plan.label(&p)));
    } else {
        done(&h, &format!("{}: {} is predecessor of {}", rid, c.plan.label(&p), c.plan.label(&s)));
    }
    note_loops(&c.store, &c.plan, before)
}

pub fn require(from: String, on: String, reason: String) -> Result<()> {
    let mut c = open()?;
    let f = c.plan.resolve(&from)?;
    let o = c.plan.resolve(&on)?;
    let rid = add_req(&mut c, Requirer::Task(f.clone()), o.clone(), reason.clone(), false, false, None);
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Required, format!("require {} of {}: {}", c.plan.number_of(&f), c.plan.number_of(&o), reason), vec![f.clone(), o.clone()]))?;
    done(&h, &format!("{}: {} requires of {}: {}", rid, c.plan.label(&f), c.plan.label(&o), reason));
    Ok(())
}

pub fn by(task: String, person: String, reason: String) -> Result<()> {
    let mut c = open()?;
    let id = c.plan.resolve(&task)?;
    let rid = add_req(&mut c, Requirer::Person(person.clone()), id.clone(), reason.clone(), false, false, None);
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Required, format!("{} requires {}: {}", person, c.plan.number_of(&id), reason), vec![id.clone()]))?;
    done(&h, &format!("{}: {} required by {}", rid, c.plan.label(&id), person));
    Ok(())
}

pub fn move_req(req: String, from: Option<String>, on: Option<String>) -> Result<()> {
    let mut c = open()?;
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
    done(&h, &format!("moved {}: {}", req, req_desc(&c.plan, &r)));
    note_loops(&c.store, &c.plan, before)
}

pub fn unlink(req: String) -> Result<()> {
    let mut c = open()?;
    let r = c.plan.reqs.remove(&req).ok_or_else(|| anyhow!("no requirement {}", req))?;
    let mut subjects = vec![r.on.clone()];
    if let Requirer::Task(t) = &r.from {
        subjects.push(t.clone());
    }
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Unlinked, format!("unlink {}: {}", req, r.reason), subjects))?;
    done(&h, &format!("removed {}", req));
    Ok(())
}

// ----- types ---------------------------------------------------------------

pub fn type_add(id: String, title: String, body: Option<String>) -> Result<()> {
    let mut c = open()?;
    if c.plan.types.contains_key(&id) {
        bail!("type {} exists", id);
    }
    let body = read_text(body, false)?.unwrap_or_default();
    c.plan.types.insert(id.clone(), TaskType { id: id.clone(), title: title.clone(), body, items: vec![] });
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::TypeAdded, format!("type {}: {}", id, title), vec![]))?;
    done(&h, &format!("added type {} ({})", id, title));
    Ok(())
}

fn parse_item_req(s: &str) -> Result<ItemReq> {
    let (blocking, s) = match s.strip_prefix('~') {
        Some(rest) => (false, rest),
        None => (true, s),
    };
    let (on, reason) = s.split_once(':').ok_or_else(|| anyhow!("`--req` wants `<core|item:<id>|task>: reason`, got `{}`", s))?;
    Ok(ItemReq { on: on.trim().to_string(), reason: reason.trim().to_string(), blocking })
}

fn instances_of(plan: &Plan, ty: &str, item: &str) -> Vec<TaskId> {
    let mut v: Vec<TaskId> = plan
        .tasks
        .values()
        .filter(|t| matches!(&t.from_item, Some((ty2, it)) if ty2 == ty && it == item))
        .map(|t| t.id.clone())
        .collect();
    v.sort();
    v
}

fn carriers(plan: &Plan, ty: &str) -> Vec<TaskId> {
    let mut v: Vec<TaskId> = plan.tasks.values().filter(|t| t.types.iter().any(|x| x == ty)).map(|t| t.id.clone()).collect();
    v.sort_by_key(|t| plan.tasks[t].seq);
    v
}

pub fn type_item(ty: String, id: String, title: String, body: Option<String>, pending: Option<String>, reqs: Vec<String>) -> Result<()> {
    let mut c = open()?;
    let body = read_text(body, false)?.unwrap_or_default();
    let reqs: Vec<ItemReq> = reqs.iter().map(|s| parse_item_req(s)).collect::<Result<_>>()?;
    if let Some(p) = &pending {
        c.plan.resolve(p)?;
    }
    let t = c.plan.types.get_mut(&ty).ok_or_else(|| anyhow!("no type {}", ty))?;
    if t.items.iter().any(|i| i.id == id) {
        bail!("type {} already has item {}", ty, id);
    }
    t.items.push(TypeItem { id: id.clone(), title: title.clone(), body, pending, reqs });
    let affected = carriers(&c.plan, &ty);
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::TypeEdited, format!("type {} item {}: {}", ty, id, title), affected.clone()))?;
    done(&h, &format!("added item {} to type {}", id, ty));
    if !affected.is_empty() {
        println!("{} task(s) carry this type and now owe this item; review each and `tsk amend` it:", affected.len());
        for a in affected {
            println!("  {}", c.plan.label(&a));
        }
    }
    Ok(())
}

pub fn type_edit_item(ty: String, item: String, title: Option<String>, body: Option<String>) -> Result<()> {
    let mut c = open()?;
    let body = read_text(body, false)?;
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
    done(&h, &format!("edited item {} of type {}", item, ty));
    if !inst.is_empty() {
        println!("{} instance(s) carry this item's scope; review each, `tsk edit` it, `tsk uncomplete` it if it must reopen:", inst.len());
        for i in inst {
            println!("  {} {}", c.derived.status(&i).glyph(), c.plan.label(&i));
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
    println!("items:");
    for i in &t.items {
        let p = i.pending.as_ref().map(|p| format!(" (pending {})", p)).unwrap_or_default();
        println!("  {}: {}{}", i.id, i.title, p);
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
    let mut c = open()?;
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
    let core = c.plan.new_task_id();
    let seq = c.plan.next_task;
    c.plan.tasks.insert(core.clone(), Task { id: core.clone(), title: format!("{} (core)", host.title), parent: Some(id.clone()), seq, body: host.body.clone(), types: vec![], from_item: None, core_of: Some(id.clone()), narrative: None, priority: None, due: None });
    subjects.push(core.clone());
    // one child per item
    let mut item_ids: Vec<(String, TaskId)> = Vec::new();
    for it in &t.items {
        let cid = c.plan.new_task_id();
        let seq = c.plan.next_task;
        c.plan.tasks.insert(cid.clone(), Task { id: cid.clone(), title: it.title.clone(), parent: Some(id.clone()), seq, body: it.body.clone(), types: vec![], from_item: Some((ty.clone(), it.id.clone())), core_of: None, narrative: None, priority: None, due: None });
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
    done(&hash, &format!("applied {} to {}: {} children", ty, c.plan.label(&id), n));
    for ch in c.plan.children(&id) {
        println!("  {}", c.plan.label(&ch.id));
    }
    Ok(())
}

// ----- state changes ------------------------------------------------------

pub fn spike(task: String) -> Result<()> {
    let c = open()?;
    let id = c.plan.resolve(&task)?;
    check_leaf_open(&c, &id, "spike")?;
    if c.derived.status(&id) == Status::Spike {
        bail!("{} is already a spike", c.plan.label(&id));
    }
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::FlaggedAsSpike, format!("spike {}", c.plan.number_of(&id)), vec![id.clone()]))?;
    done(&h, &format!("flagged {} as a spike: prove it, then break it down", c.plan.label(&id)));
    Ok(())
}

pub fn confirm(task: String) -> Result<()> {
    let mut c = open()?;
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
    done(&h, &format!("confirmed {}: it is real work now", c.plan.label(&id)));
    Ok(())
}

/// The loop-group rule shared by start and complete. Returns the blocks
/// that the group does not cover.
fn uncovered(c: &Ctx, id: &str, group: &BTreeSet<TaskId>, completing: bool) -> Vec<String> {
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
    let c = open()?;
    if tasks.is_empty() {
        bail!("name at least one task");
    }
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
        for u in uncovered(&c, id, &group, completing) {
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
    done(&h, &format!("{}{}: {}", if !problems.is_empty() { "FORCED " } else { "" }, kind.name(), labels.join("; ")));
    if completing {
        // Say what just became ready.
        let history = c.store.history()?;
        let d = Derived::compute(&c.plan, &history);
        let newly: Vec<TaskId> = d.ready(&c.plan).into_iter().filter(|t| !c.derived.is_ready(&c.plan, t)).collect();
        if !newly.is_empty() {
            println!("now ready:");
            for t in newly {
                println!("  {}", c.plan.label(&t));
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
    let c = open()?;
    let id = c.plan.resolve(&task)?;
    if c.derived.status(&id) != Status::InProgress {
        bail!("{} is not in progress", c.plan.label(&id));
    }
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Paused, format!("pause {}", c.plan.number_of(&id)), vec![id.clone()]))?;
    done(&h, &format!("paused {}", c.plan.label(&id)));
    Ok(())
}

pub fn uncomplete(task: String) -> Result<()> {
    let c = open()?;
    let id = c.plan.resolve(&task)?;
    if c.plan.is_parent(&id) {
        bail!("{} is a parent; uncomplete one of its children", c.plan.label(&id));
    }
    if c.derived.status(&id) != Status::Complete {
        bail!("{} is not complete", c.plan.label(&id));
    }
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Uncompleted, format!("uncomplete {}", c.plan.number_of(&id)), vec![id.clone()]))?;
    done(&h, &format!("reopened {}", c.plan.label(&id)));
    Ok(())
}

pub fn drop(task: String, reason: String) -> Result<()> {
    let c = open()?;
    let id = c.plan.resolve(&task)?;
    if c.derived.status(&id) == Status::Dropped {
        bail!("{} is already dropped", c.plan.label(&id));
    }
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Dropped, format!("drop {}: {}", c.plan.number_of(&id), reason), vec![id.clone()]))?;
    done(&h, &format!("dropped {} and everything under it: {}", c.plan.label(&id), reason));
    Ok(())
}

// ----- views ---------------------------------------------------------------

fn block_text(c: &Ctx, b: &Block) -> String {
    match b {
        Block::Loop(i) => {
            let lp = &c.derived.loops[*i];
            format!("in loop #{} with {}", i + 1, lp.tasks.iter().map(|t| c.plan.number_of(t)).collect::<Vec<_>>().join(", "))
        }
        Block::Tentative(r, met) => {
            if *met {
                format!("tentative; {} is complete, so confirm or drop ({})", c.plan.label(&c.plan.reqs[r].on), r)
            } else {
                format!("tentative until {} ({})", c.plan.label(&c.plan.reqs[r].on), r)
            }
        }
        Block::Waiting(p, r, n) => {
            let under = if c.plan.is_parent(p) { format!(", {} open under it", n) } else { String::new() };
            format!("waits on {} ({}: {}{})", c.plan.label(p), r, c.plan.reqs[r].reason, under)
        }
    }
}

pub fn show(task: String, json: bool) -> Result<()> {
    let c = open()?;
    let id = c.plan.resolve(&task)?;
    let t = c.plan.task(&id)?;
    let st = c.derived.status(&id);
    let blocks = c.derived.blocks(&c.plan, &id);
    let events: Vec<&(Hash, Event)> = c.history.iter().filter(|(_, e)| e.subjects.contains(&id)).collect();
    if json {
        let v = json!({
            "task": t,
            "number": c.plan.number_of(&id),
            "status": st.name(),
            "ready": c.derived.is_ready(&c.plan, &id),
            "tentative": c.derived.is_tentative(&c.plan, &id),
            "path": c.plan.ancestors_and_self(&id).iter().map(|a| a.id.clone()).collect::<Vec<_>>(),
            "children": c.plan.children(&id).iter().map(|k| json!({"id": k.id, "number": c.plan.number_of(&k.id), "title": k.title, "status": c.derived.status(&k.id).name()})).collect::<Vec<_>>(),
            "required_by": c.plan.reqs_on(&id),
            "requires": c.plan.reqs_from(&id),
            "blocks": blocks.iter().map(|b| block_text(&c, b)).collect::<Vec<_>>(),
            "events": events.iter().map(|(h, e)| json!({"hash": h, "kind": e.kind.name(), "time": e.time, "author": e.author, "message": e.message, "forced": e.forced})).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!("{} {}  {}", st.glyph(), c.plan.number_of(&id), t.title);
    let mut facts = vec![format!("id {}", id), format!("status {}", st.name())];
    if !t.types.is_empty() {
        facts.push(format!("type {}", t.types.join(",")));
    }
    if st == Status::Dropped {
        if let Some((_, d)) = c.history.iter().find(|(_, e)| e.subjects.contains(&id) && e.kind == EventKind::Dropped) {
            facts.push(d.message.clone());
        }
    }
    if let Some((ty, it)) = &t.from_item {
        facts.push(format!("from item {}/{}", ty, it));
    }
    if t.core_of.is_some() {
        facts.push("core".to_string());
    }
    if c.derived.is_tentative(&c.plan, &id) {
        facts.push("tentative".to_string());
    }
    if let Some(p) = &t.priority {
        facts.push(format!("priority {}", p));
    }
    if let Some(d) = &t.due {
        facts.push(format!("due {}", d));
    }
    println!("    {}", facts.join(" · "));
    let path = c.plan.ancestors_and_self(&id);
    if path.len() > 1 {
        println!("\npath:");
        for a in &path[..path.len() - 1] {
            println!("  {} {}", c.plan.number_of(&a.id), a.title);
        }
    }
    if !t.body.is_empty() {
        println!();
        for l in t.body.lines() {
            println!("  {}", l);
        }
    }
    let on = c.plan.reqs_on(&id);
    if !on.is_empty() {
        println!("\nrequired by:");
        for r in on {
            let k = if r.conditional { "conditional" } else if r.blocking { "successor" } else { "technical" };
            println!("  {} {} [{}]: {}", r.id, k, c.plan.requirer_label(&r.from), r.reason);
        }
    }
    let from = c.plan.reqs_from(&id);
    if !from.is_empty() {
        println!("\nrequires:");
        for r in from {
            let k = if r.conditional { "conditional on" } else if r.blocking { "predecessor" } else { "technical, of" };
            println!("  {} {} [{}]: {}", r.id, k, c.plan.label(&r.on), r.reason);
        }
    }
    let mut inherited = Vec::new();
    for a in &path[..path.len().saturating_sub(1)] {
        for r in c.plan.reqs_from(&a.id) {
            if r.blocking {
                inherited.push(format!("  {} via {}: predecessor [{}]: {}", r.id, c.plan.number_of(&a.id), c.plan.label(&r.on), r.reason));
            }
        }
    }
    if !inherited.is_empty() {
        println!("\ninherited:");
        for l in inherited {
            println!("{}", l);
        }
    }
    let kids = c.plan.children(&id);
    if !kids.is_empty() {
        println!("\nbreakdown:");
        if let Some(n) = &t.narrative {
            for l in n.lines() {
                println!("  {}", l);
            }
        }
        for k in kids {
            println!("  {} {} {}", c.derived.status(&k.id).glyph(), c.plan.number_of(&k.id), k.title);
        }
    } else if !blocks.is_empty() && st != Status::Complete && st != Status::Dropped {
        println!("\nblocked:");
        for b in &blocks {
            println!("  {}", block_text(&c, b));
        }
    } else if c.derived.is_ready(&c.plan, &id) {
        println!("\nready to start");
    }
    if !events.is_empty() {
        println!("\nhistory:");
        for (h, e) in events.iter().take(8) {
            println!("  {}  {}  {}{}", &h[..10], e.time, e.kind.name(), if e.forced { " (forced)" } else { "" });
        }
    }
    Ok(())
}

pub fn plan(task: Option<String>, all: bool, json: bool) -> Result<()> {
    let c = open()?;
    let roots: Vec<TaskId> = match task {
        Some(t) => vec![c.plan.resolve(&t)?],
        None => c.plan.roots().iter().map(|t| t.id.clone()).collect(),
    };
    fn walk(c: &Ctx, id: &TaskId, depth: usize, all: bool, out: &mut Vec<serde_json::Value>, text: &mut String) {
        let st = c.derived.status(id);
        if !all && matches!(st, Status::Complete | Status::Dropped) {
            return;
        }
        let t = &c.plan.tasks[id];
        let mut tags = Vec::new();
        if c.derived.is_ready(&c.plan, id) {
            tags.push("ready");
        }
        if c.derived.in_loop.contains_key(id) {
            tags.push("loop");
        }
        if c.derived.is_tentative(&c.plan, id) {
            tags.push("tentative");
        }
        let tag = if tags.is_empty() { String::new() } else { format!("  <{}>", tags.join(", ")) };
        text.push_str(&format!("{}{} {} {}{}\n", "    ".repeat(depth), st.glyph(), c.plan.number_of(id), t.title, tag));
        out.push(json!({"id": id, "number": c.plan.number_of(id), "title": t.title, "status": st.name(), "depth": depth, "tags": tags}));
        for k in c.plan.children(id) {
            walk(c, &k.id, depth + 1, all, out, text);
        }
    }
    let mut out = Vec::new();
    let mut text = String::new();
    for r in &roots {
        walk(&c, r, 0, all, &mut out, &mut text);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else if text.is_empty() {
        println!("(no open tasks)");
    } else {
        print!("{}", text);
    }
    Ok(())
}

pub fn ready(blocked: bool, json: bool) -> Result<()> {
    let c = open()?;
    let ready = c.derived.ready(&c.plan);
    let mut blocked_list: Vec<(TaskId, Vec<Block>)> = Vec::new();
    if blocked || json {
        let mut leaves: Vec<&Task> = c.plan.tasks.values().filter(|t| !c.plan.is_parent(&t.id)).collect();
        leaves.sort_by_key(|t| t.seq);
        for t in leaves {
            let st = c.derived.status(&t.id);
            if matches!(st, Status::Todo | Status::Spike | Status::Paused) && !c.derived.is_ready(&c.plan, &t.id) {
                blocked_list.push((t.id.clone(), c.derived.blocks(&c.plan, &t.id)));
            }
        }
    }
    if json {
        let v = json!({
            "ready": ready.iter().map(|t| json!({"id": t, "number": c.plan.number_of(t), "title": c.plan.tasks[t].title, "status": c.derived.status(t).name()})).collect::<Vec<_>>(),
            "blocked": blocked_list.iter().map(|(t, bs)| json!({"id": t, "number": c.plan.number_of(t), "title": c.plan.tasks[t].title, "blocks": bs.iter().map(|b| block_text(&c, b)).collect::<Vec<_>>()})).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    if ready.is_empty() {
        println!("nothing is ready");
    } else {
        println!("ready:");
        for t in &ready {
            println!("  {} {}", c.derived.status(t).glyph(), c.plan.label(t));
        }
    }
    if blocked {
        if blocked_list.is_empty() {
            println!("nothing is blocked");
        } else {
            println!("blocked:");
            for (t, bs) in &blocked_list {
                println!("  {} {}", c.derived.status(t).glyph(), c.plan.label(t));
                for b in bs {
                    println!("      {}", block_text(&c, b));
                }
            }
        }
    }
    Ok(())
}

pub fn loops(json: bool) -> Result<()> {
    let c = open()?;
    if json {
        let v: Vec<_> = c
            .derived
            .loops
            .iter()
            .map(|l| json!({"tasks": l.tasks, "reqs": l.reqs.iter().map(|r| &c.plan.reqs[r]).collect::<Vec<_>>()}))
            .collect();
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    if c.derived.loops.is_empty() {
        println!("no loops");
        return Ok(());
    }
    for (i, l) in c.derived.loops.iter().enumerate() {
        println!("loop #{}:", i + 1);
        for t in &l.tasks {
            println!("  {} {}", c.derived.status(t).glyph(), c.plan.label(t));
        }
        println!("  formed by:");
        for r in &l.reqs {
            let req = &c.plan.reqs[r];
            println!("    {} {} -> {}: {}", r, c.plan.number_of(&req.on), match &req.from { Requirer::Task(t) => c.plan.number_of(t), Requirer::Person(p) => p.clone() }, req.reason);
        }
    }
    println!("\nto break a loop: break down one of its tasks and send each requirement to the child it belongs to;");
    println!("add a common predecessor; turn a requirement into a technical one (`tsk move`, `tsk unlink` + `tsk require`);");
    println!("or, if the tasks are small, finish them together: `tsk complete <all of them>`.");
    Ok(())
}

pub fn stale(json: bool) -> Result<()> {
    let c = open()?;
    let mut rows: Vec<(String, String, String, TaskId)> = Vec::new(); // date, person, reason, task
    let mut unowned: Vec<TaskId> = Vec::new();
    let mut open_tasks: Vec<&Task> = c.plan.tasks.values().filter(|t| !matches!(c.derived.status(&t.id), Status::Complete | Status::Dropped)).collect();
    open_tasks.sort_by_key(|t| t.seq);
    for t in open_tasks {
        let mut any = false;
        for a in c.plan.ancestors_and_self(&t.id) {
            for r in c.plan.reqs_on(&a.id) {
                if let Requirer::Person(p) = &r.from {
                    if a.id == t.id {
                        rows.push((r.created.clone(), p.clone(), r.reason.clone(), t.id.clone()));
                    }
                    any = true;
                }
            }
        }
        if !any && t.parent.is_none() {
            unowned.push(t.id.clone());
        }
    }
    rows.sort();
    if json {
        println!("{}", serde_json::to_string_pretty(&json!({"required": rows.iter().map(|(d, p, r, t)| json!({"date": d, "person": p, "reason": r, "task": t})).collect::<Vec<_>>(), "unowned_roots": unowned}))?);
        return Ok(());
    }
    if rows.is_empty() && unowned.is_empty() {
        println!("nothing open");
        return Ok(());
    }
    if !rows.is_empty() {
        println!("open work, oldest requirement first:");
        for (d, p, r, t) in &rows {
            println!("  {}  {}  {}: {}", &d[..10], c.plan.label(t), p, r);
        }
    }
    if !unowned.is_empty() {
        println!("roots nobody has asked for:");
        for t in unowned {
            println!("  {}", c.plan.label(&t));
        }
    }
    Ok(())
}

pub fn forced(json: bool) -> Result<()> {
    let c = open()?;
    let rows: Vec<&(Hash, Event)> = c.history.iter().filter(|(_, e)| e.forced).collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&rows.iter().map(|(h, e)| json!({"hash": h, "event": e})).collect::<Vec<_>>())?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("no forced events");
        return Ok(());
    }
    println!("forced events (each one says the process or the tool needs a look):");
    for (h, e) in rows {
        println!("  {}  {}  {}  {}", &h[..10], e.time, e.kind.name(), e.message);
    }
    Ok(())
}

pub fn log(task: Option<String>, limit: Option<usize>, json: bool) -> Result<()> {
    let c = open()?;
    let filter = match task {
        Some(t) => Some(c.plan.resolve(&t)?),
        None => None,
    };
    let rows: Vec<&(Hash, Event)> = c
        .history
        .iter()
        .filter(|(_, e)| filter.as_ref().map(|f| e.subjects.contains(f)).unwrap_or(true))
        .take(limit.unwrap_or(usize::MAX))
        .collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&rows.iter().map(|(h, e)| json!({"hash": h, "event": e})).collect::<Vec<_>>())?);
        return Ok(());
    }
    for (h, e) in rows {
        let subj = if e.subjects.is_empty() { String::new() } else { format!("  [{}]", e.subjects.iter().map(|s| c.plan.number_of(s)).collect::<Vec<_>>().join(" ")) };
        println!("{}  {}  {:<16} {}{}{}", &h[..10], e.time, e.kind.name(), e.message, subj, if e.forced { "  FORCED" } else { "" });
    }
    Ok(())
}

pub fn diff(task: String, from: Option<String>, to: Option<String>) -> Result<()> {
    let c = open()?;
    let id = c.plan.resolve(&task)?;
    let touching: Vec<&(Hash, Event)> = c.history.iter().filter(|(_, e)| e.subjects.contains(&id)).collect();
    let (new_h, old_h) = match (from, to) {
        (Some(f), Some(t)) => (c.store.resolve_hash(&t)?, c.store.resolve_hash(&f)?),
        (Some(f), None) => (c.store.head()?.unwrap(), c.store.resolve_hash(&f)?),
        (None, _) => {
            if touching.len() < 2 {
                bail!("{} has only one event; nothing to diff", c.plan.label(&id));
            }
            (touching[0].0.clone(), touching[1].0.clone())
        }
    };
    let load = |h: &Hash| -> Result<Option<Task>> {
        let ev: Event = c.store.read_object(ObjectType::Event, h)?;
        let snap: Snapshot = c.store.read_object(ObjectType::Snapshot, &ev.snapshot)?;
        Ok(match snap.tasks.get(&id) {
            Some(th) => Some(c.store.read_object(ObjectType::Task, th)?),
            None => None,
        })
    };
    let old = load(&old_h)?;
    let new = load(&new_h)?;
    println!("--- {} @ {}", id, &old_h[..10]);
    println!("+++ {} @ {}", id, &new_h[..10]);
    let render = |t: &Option<Task>| -> String {
        match t {
            None => String::from("(absent)\n"),
            Some(t) => format!("title: {}\nparent: {}\ntypes: {}\nnarrative: {}\n---\n{}\n", t.title, t.parent.clone().unwrap_or_default(), t.types.join(","), t.narrative.clone().unwrap_or_default(), t.body),
        }
    };
    for l in line_diff(&render(&old), &render(&new)) {
        println!("{}", l);
    }
    Ok(())
}

/// Minimal LCS line diff.
fn line_diff(a: &str, b: &str) -> Vec<String> {
    let a: Vec<&str> = a.lines().collect();
    let b: Vec<&str> = b.lines().collect();
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] { dp[i + 1][j + 1] + 1 } else { dp[i + 1][j].max(dp[i][j + 1]) };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < n && j < m {
        if a[i] == b[j] {
            out.push(format!("  {}", a[i]));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            out.push(format!("- {}", a[i]));
            i += 1;
        } else {
            out.push(format!("+ {}", b[j]));
            j += 1;
        }
    }
    while i < n {
        out.push(format!("- {}", a[i]));
        i += 1;
    }
    while j < m {
        out.push(format!("+ {}", b[j]));
        j += 1;
    }
    out
}

pub fn config(key: Option<String>, value: Option<String>) -> Result<()> {
    let store = Store::discover(&std::env::current_dir()?)?;
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

// ===== plumbing ============================================================

pub fn cat_object(hash: String) -> Result<()> {
    let store = Store::discover(&std::env::current_dir()?)?;
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
    let store = Store::discover(&std::env::current_dir()?)?;
    let h = match event {
        Some(e) => store.resolve_hash(&e)?,
        None => store.head()?.ok_or_else(|| anyhow!("no HEAD"))?,
    };
    let ev: Event = store.read_object(ObjectType::Event, &h)?;
    let snap: Snapshot = store.read_object(ObjectType::Snapshot, &ev.snapshot)?;
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
    let store = Store::discover(&std::env::current_dir()?)?;
    for (h, _) in store.history()?.iter().take(limit.unwrap_or(usize::MAX)) {
        println!("{}", h);
    }
    Ok(())
}
