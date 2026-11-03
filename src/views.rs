//! Read-only commands: the card, the chain of intent, the outline, readiness,
//! loops, review views, the log, and diffs.

use crate::commands::{open, Ctx};
use crate::model::*;
use crate::plan::Plan;
use crate::state::{Block, Derived, Status};
use crate::store::Store;
use anyhow::{anyhow, bail, Context as _, Result};
use serde_json::json;
use std::collections::BTreeSet;

pub(crate) fn block_text(c: &Ctx, b: &Block) -> String {
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

/// `key` or `key=value` filters from `--where`.
fn parse_filters(filters: &[String]) -> Vec<(String, Option<String>)> {
    filters
        .iter()
        .map(|f| match f.split_once('=') {
            Some((k, v)) => (k.trim().to_string(), Some(v.trim().to_string())),
            None => (f.trim().to_string(), None),
        })
        .collect()
}

fn fields_text(f: &Fields) -> String {
    f.iter().map(|(k, v)| format!("{}={}", k, v)).collect::<Vec<_>>().join(" ")
}

fn person_reqs(plan: &Plan, id: &str) -> Vec<String> {
    plan.reqs_on(id)
        .iter()
        .filter_map(|r| match &r.from {
            Requirer::Person(p) => Some(format!("{}: {}", p, r.reason)),
            _ => None,
        })
        .collect()
}

pub fn show(task: String, json: bool) -> Result<()> {
    let c = open()?;
    let id = c.plan.resolve(&task)?;
    let t = c.plan.task(&id)?;
    let st = c.derived.status(&id);
    let blocks = c.derived.blocks(&c.plan, &id);
    let events: Vec<&(Hash, Event)> = c.history().iter().filter(|(_, e)| e.subjects.contains(&id)).collect();
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
        if let Some((_, d)) = c.history().iter().find(|(_, e)| e.subjects.contains(&id) && e.kind == EventKind::Dropped) {
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
    if !t.fields.is_empty() {
        println!("    fields: {}", fields_text(&t.fields));
    }
    if !t.docs.is_empty() {
        println!("    docs: {}", t.docs.join(", "));
    }
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
            println!("  {} {} [{}]: {}{}", r.id, k, c.plan.requirer_label(&r.from), r.reason, req_fields(r));
        }
    }
    let from = c.plan.reqs_from(&id);
    if !from.is_empty() {
        println!("\nrequires:");
        for r in from {
            let k = if r.conditional { "conditional on" } else if r.blocking { "predecessor" } else { "technical, of" };
            println!("  {} {} [{}]: {}{}", r.id, k, c.plan.label(&r.on), r.reason, req_fields(r));
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

fn req_fields(r: &Requirement) -> String {
    if r.fields.is_empty() {
        String::new()
    } else {
        format!("  {{{}}}", fields_text(&r.fields))
    }
}

/// The chain of intent: what an agent assigned to a task reads first. Root
/// to task, each level with who asked and why, its narrative (how its
/// children fit, which is why this task exists), its docs and fields; then
/// the task's own card, requirements, siblings and children.
pub fn context(task: String, full: bool, json: bool) -> Result<()> {
    let c = open()?;
    let id = c.plan.resolve(&task)?;
    let t = c.plan.task(&id)?;
    let st = c.derived.status(&id);
    let chain = c.plan.ancestors_and_self(&id);
    let blocks = c.derived.blocks(&c.plan, &id);
    let siblings: Vec<&Task> = match &t.parent {
        Some(p) => c.plan.children(p).into_iter().filter(|s| s.id != id).collect(),
        None => vec![],
    };
    let kids = c.plan.children(&id);
    // Every requirement the task must honour: own, plus blocking ones inherited.
    let mut inherited: Vec<(&Task, &Requirement)> = Vec::new();
    // And what other tasks require of its ancestors: who waits on this work.
    let mut inherited_on: Vec<(&Task, &Requirement)> = Vec::new();
    for a in &chain[..chain.len() - 1] {
        for r in c.plan.reqs_from(&a.id) {
            if r.blocking {
                inherited.push((a, r));
            }
        }
        for r in c.plan.reqs_on(&a.id) {
            if r.blocking && matches!(r.from, Requirer::Task(_)) {
                inherited_on.push((a, r));
            }
        }
    }
    if json {
        let v = json!({
            "task": t,
            "number": c.plan.number_of(&id),
            "status": st.name(),
            "ready": c.derived.is_ready(&c.plan, &id),
            "blocks": blocks.iter().map(|b| block_text(&c, b)).collect::<Vec<_>>(),
            "chain": chain.iter().map(|a| json!({
                "id": a.id, "number": c.plan.number_of(&a.id), "title": a.title,
                "required_by": person_reqs(&c.plan, &a.id),
                "narrative": a.narrative, "body": a.body, "docs": a.docs, "fields": a.fields,
                "status": c.derived.status(&a.id).name(),
            })).collect::<Vec<_>>(),
            "requires": c.plan.reqs_from(&id),
            "required_by": c.plan.reqs_on(&id),
            "inherited": inherited.iter().map(|(a, r)| json!({"via": a.id, "req": r})).collect::<Vec<_>>(),
            "inherited_required_by": inherited_on.iter().map(|(a, r)| json!({"via": a.id, "req": r})).collect::<Vec<_>>(),
            "siblings": siblings.iter().map(|s| json!({"id": s.id, "number": c.plan.number_of(&s.id), "title": s.title, "status": c.derived.status(&s.id).name()})).collect::<Vec<_>>(),
            "children": kids.iter().map(|k| json!({"id": k.id, "number": c.plan.number_of(&k.id), "title": k.title, "status": c.derived.status(&k.id).name()})).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!("context for {} {}  {}", c.plan.number_of(&id), id, t.title);
    let mut facts = vec![format!("status {}", st.name())];
    if c.derived.is_ready(&c.plan, &id) {
        facts.push("ready".into());
    }
    if c.derived.is_tentative(&c.plan, &id) {
        facts.push("tentative".into());
    }
    println!("    {}", facts.join(" · "));
    println!("\nchain of intent, root first:");
    for (depth, a) in chain.iter().enumerate() {
        let pad = "  ".repeat(depth + 1);
        let here = if a.id == id { "  <- this task" } else { "" };
        println!("{}{} {} {}{}", pad, c.derived.status(&a.id).glyph(), c.plan.number_of(&a.id), a.title, here);
        for p in person_reqs(&c.plan, &a.id) {
            println!("{}  required by {}", pad, p);
        }
        if !a.fields.is_empty() {
            println!("{}  fields: {}", pad, fields_text(&a.fields));
        }
        for d in &a.docs {
            println!("{}  doc: {}", pad, d);
        }
        if a.id != id {
            if full && !a.body.is_empty() {
                for l in a.body.lines() {
                    println!("{}  | {}", pad, l);
                }
            }
            if let Some(n) = &a.narrative {
                println!("{}  narrative:", pad);
                for l in n.lines().filter(|l| !l.trim().is_empty()) {
                    println!("{}    {}", pad, l);
                }
            }
        }
    }
    if !t.body.is_empty() {
        println!("\ncard:");
        for l in t.body.lines() {
            println!("  {}", l);
        }
    }
    if let Some(n) = &t.narrative {
        println!("\nnarrative:");
        for l in n.lines() {
            println!("  {}", l);
        }
    }
    let from = c.plan.reqs_from(&id);
    if !from.is_empty() || !inherited.is_empty() {
        println!("\nthis task requires:");
        for r in from {
            let k = if r.conditional { "conditional on" } else if r.blocking { "predecessor" } else { "technical, of" };
            let st = c.derived.status(&r.on);
            println!("  {} {} [{}] ({}): {}", r.id, k, c.plan.label(&r.on), st.name(), r.reason);
        }
        for (a, r) in &inherited {
            println!("  {} via {}: predecessor [{}] ({}): {}", r.id, c.plan.number_of(&a.id), c.plan.label(&r.on), c.derived.status(&r.on).name(), r.reason);
        }
    }
    let on: Vec<&Requirement> = c.plan.reqs_on(&id).into_iter().filter(|r| matches!(r.from, Requirer::Task(_))).collect();
    if !on.is_empty() || !inherited_on.is_empty() {
        println!("\nrequired of this task:");
        for r in on {
            let k = if r.conditional { "conditional" } else if r.blocking { "successor" } else { "technical" };
            println!("  {} {} [{}]: {}", r.id, k, c.plan.requirer_label(&r.from), r.reason);
        }
        for (a, r) in &inherited_on {
            println!("  {} via {}: successor [{}]: {}", r.id, c.plan.number_of(&a.id), c.plan.requirer_label(&r.from), r.reason);
        }
    }
    if !blocks.is_empty() && matches!(st, Status::Todo | Status::Spike | Status::Paused) {
        println!("\nblocked:");
        for b in &blocks {
            println!("  {}", block_text(&c, b));
        }
    }
    if !siblings.is_empty() {
        println!("\nsiblings:");
        for s in siblings {
            println!("  {} {} {}", c.derived.status(&s.id).glyph(), c.plan.number_of(&s.id), s.title);
        }
    }
    if !kids.is_empty() {
        println!("\nchildren:");
        for k in kids {
            println!("  {} {} {}", c.derived.status(&k.id).glyph(), c.plan.number_of(&k.id), k.title);
        }
    }
    Ok(())
}

pub fn plan(task: Option<String>, all: bool, filters: Vec<String>, json: bool) -> Result<()> {
    let c = open()?;
    let filters = parse_filters(&filters);
    let roots: Vec<TaskId> = match task {
        Some(t) => vec![c.plan.resolve(&t)?],
        None => c.plan.roots().iter().map(|t| t.id.clone()).collect(),
    };
    fn walk(c: &Ctx, id: &TaskId, depth: usize, all: bool, filters: &[(String, Option<String>)], out: &mut Vec<serde_json::Value>, text: &mut String) {
        let st = c.derived.status(id);
        if !all && matches!(st, Status::Complete | Status::Dropped) {
            return;
        }
        if !filters.is_empty() && !c.plan.subtree(id).iter().any(|d| c.plan.matches(d, filters)) {
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
        out.push(json!({"id": id, "number": c.plan.number_of(id), "title": t.title, "status": st.name(), "depth": depth, "tags": tags, "fields": t.fields}));
        for k in c.plan.children(id) {
            walk(c, &k.id, depth + 1, all, filters, out, text);
        }
    }
    let mut out = Vec::new();
    let mut text = String::new();
    for r in &roots {
        walk(&c, r, 0, all, &filters, &mut out, &mut text);
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

pub fn ready(blocked: bool, filters: Vec<String>, json: bool) -> Result<()> {
    let c = open()?;
    let filters = parse_filters(&filters);
    let ready: Vec<TaskId> = c.derived.ready(&c.plan).into_iter().filter(|t| c.plan.matches(t, &filters)).collect();
    let mut blocked_list: Vec<(TaskId, Vec<Block>)> = Vec::new();
    if blocked || json {
        let mut leaves: Vec<&Task> = c.plan.tasks.values().filter(|t| !c.plan.is_parent(&t.id)).collect();
        leaves.sort_by_key(|t| t.seq);
        for t in leaves {
            let st = c.derived.status(&t.id);
            if matches!(st, Status::Todo | Status::Spike | Status::Paused) && !c.derived.is_ready(&c.plan, &t.id) && c.plan.matches(&t.id, &filters) {
                blocked_list.push((t.id.clone(), c.derived.blocks(&c.plan, &t.id)));
            }
        }
    }
    if json {
        let v = json!({
            "ready": ready.iter().map(|t| json!({"id": t, "number": c.plan.number_of(t), "title": c.plan.tasks[t].title, "status": c.derived.status(t).name(), "fields": c.plan.tasks[t].fields})).collect::<Vec<_>>(),
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
    let rows: Vec<&(Hash, Event)> = c.history().iter().filter(|(_, e)| e.forced).collect();
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
        .history()
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
        println!("{}  {}  {:<16} {}{}{}  -- {}", &h[..10], e.time, e.kind.name(), e.message, subj, if e.forced { "  FORCED" } else { "" }, e.author);
    }
    Ok(())
}

// ----- diff ----------------------------------------------------------------

/// The store's HEAD as committed in git at `rev`, read with `git show`.
pub(crate) fn git_head_at(store: &Store, rev: &str) -> Result<Hash> {
    let root = store.root();
    let run = |args: &[&str]| -> Result<String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .output()
            .context("running git (is it installed and on PATH?)")?;
        if !out.status.success() {
            bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let prefix = run(&["rev-parse", "--show-prefix"])?;
    let path = format!("{}{}/HEAD", prefix, crate::store::DIR);
    let h = run(&["show", &format!("{}:{}", rev, path)]).with_context(|| format!("the store's HEAD is not committed at {}", rev))?;
    let h = h.lines().next().unwrap_or("").trim().to_string();
    if h.len() != 64 {
        bail!("{}:{} does not hold an event hash", rev, path);
    }
    Ok(h)
}

/// Whether the store lives in a git work tree with its HEAD committed.
fn git_head_if_any(store: &Store) -> Option<Hash> {
    git_head_at(store, "HEAD").ok()
}

pub fn diff(task: Option<String>, from: Option<String>, to: Option<String>, git: Option<String>, json: bool) -> Result<()> {
    let c = open()?;
    let head = c.store.head()?.ok_or_else(|| anyhow!("no HEAD"))?;
    let new_h = match &to {
        Some(t) => c.store.resolve_hash(t)?,
        None => head.clone(),
    };
    let mut basis = String::new();
    let old_h = match (&from, &git, &task) {
        (Some(f), _, _) => c.store.resolve_hash(f)?,
        (None, Some(rev), _) => {
            basis = format!("git {}", rev);
            git_head_at(&c.store, rev)?
        }
        (None, None, Some(t)) => {
            let id = c.plan.resolve(t)?;
            let touching: Vec<&(Hash, Event)> = c.history().iter().filter(|(_, e)| e.subjects.contains(&id)).collect();
            if touching.len() < 2 {
                bail!("{} has only one event; nothing to diff", c.plan.label(&id));
            }
            touching[1].0.clone()
        }
        (None, None, None) => match git_head_if_any(&c.store) {
            Some(h) => {
                basis = "git HEAD".into();
                h
            }
            None => {
                let ev: Event = c.store.read_object(ObjectType::Event, &new_h)?;
                basis = "the previous event".into();
                ev.parent.ok_or_else(|| anyhow!("{} is the first event; nothing to diff", &new_h[..10]))?
            }
        },
    };
    if old_h == new_h {
        if json {
            println!("{}", json!({"from": old_h, "to": new_h, "events": [], "changes": []}));
        } else {
            println!("no changes since {}{}", &old_h[..10], if basis.is_empty() { String::new() } else { format!(" ({})", basis) });
        }
        return Ok(());
    }
    match task {
        Some(t) => diff_task(&c, &c.plan.resolve(&t)?, &old_h, &new_h),
        None => diff_plan(&c, &old_h, &new_h, &basis, json),
    }
}

fn diff_task(c: &Ctx, id: &str, old_h: &Hash, new_h: &Hash) -> Result<()> {
    let load = |h: &Hash| -> Result<Option<Task>> {
        let ev: Event = c.store.read_object(ObjectType::Event, h)?;
        let snap = c.store.read_snapshot(&ev.snapshot)?;
        Ok(match snap.tasks.get(id) {
            Some(th) => Some(c.store.read_object(ObjectType::Task, th)?),
            None => None,
        })
    };
    let old = load(old_h)?;
    let new = load(new_h)?;
    println!("--- {} @ {}", id, &old_h[..10]);
    println!("+++ {} @ {}", id, &new_h[..10]);
    let render = |t: &Option<Task>| -> String {
        match t {
            None => String::from("(absent)\n"),
            Some(t) => format!(
                "title: {}\nparent: {}\ntypes: {}\ndocs: {}\nfields: {}\nnarrative: {}\n---\n{}\n",
                t.title,
                t.parent.clone().unwrap_or_default(),
                t.types.join(","),
                t.docs.join(", "),
                fields_text(&t.fields),
                t.narrative.clone().unwrap_or_default(),
                t.body
            ),
        }
    };
    for l in line_diff(&render(&old), &render(&new)) {
        println!("{}", l);
    }
    Ok(())
}

/// A plan and its derived state as of one event.
fn plan_at(store: &Store, h: &Hash) -> Result<(Plan, Derived)> {
    let ev: Event = store.read_object(ObjectType::Event, h)?;
    let snap = store.read_snapshot(&ev.snapshot).with_context(|| format!("event {}", &h[..10]))?;
    let _ = snap;
    let plan = store.load_plan(&ev.snapshot)?;
    let leaf = store.leaf_statuses_at(h)?;
    let derived = Derived::compute(&plan, &leaf);
    Ok((plan, derived))
}

fn task_changes(a: &Task, b: &Task) -> Vec<&'static str> {
    let mut v = Vec::new();
    if a.title != b.title {
        v.push("title");
    }
    if a.parent != b.parent {
        v.push("parent");
    }
    if a.body != b.body {
        v.push("body");
    }
    if a.narrative != b.narrative {
        v.push("narrative");
    }
    if a.types != b.types {
        v.push("types");
    }
    if a.docs != b.docs {
        v.push("docs");
    }
    if a.fields != b.fields {
        v.push("fields");
    }
    if a.priority != b.priority || a.due != b.due {
        v.push("priority/due");
    }
    v
}

fn diff_plan(c: &Ctx, old_h: &Hash, new_h: &Hash, basis: &str, json: bool) -> Result<()> {
    let (old, old_d) = plan_at(&c.store, old_h)?;
    let (new, new_d) = plan_at(&c.store, new_h)?;
    // Events on the new line that the old line did not hold. When old is an
    // ancestor, that is the events after it; after a merge rewrote the line,
    // an event is held if the old line had it or its original.
    let old_have: BTreeSet<Hash> = c
        .store
        .history_from(Some(old_h.clone()))?
        .into_iter()
        .flat_map(|(h, e)| std::iter::once(h).chain(e.replayed_from))
        .collect();
    let mut events: Vec<(Hash, Event)> = c
        .store
        .history_from(Some(new_h.clone()))?
        .into_iter()
        .filter(|(h, e)| !old_have.contains(h) && !e.replayed_from.as_ref().map(|r| old_have.contains(r)).unwrap_or(false))
        .collect();
    events.reverse();
    let label = |p: &Plan, id: &str| p.label(id);
    let mut changes: Vec<serde_json::Value> = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    let mut ids: Vec<&TaskId> = new.tasks.keys().chain(old.tasks.keys()).collect::<BTreeSet<_>>().into_iter().collect();
    ids.sort_by_key(|id| new.tasks.get(*id).or_else(|| old.tasks.get(*id)).map(|t| t.seq).unwrap_or(0));
    for id in ids {
        match (old.tasks.get(id), new.tasks.get(id)) {
            (None, Some(_)) => {
                lines.push(format!("+ {}", label(&new, id)));
                changes.push(json!({"task": id, "change": "added"}));
            }
            (Some(_), None) => {
                lines.push(format!("- {}", label(&old, id)));
                changes.push(json!({"task": id, "change": "removed"}));
            }
            (Some(a), Some(b)) => {
                let what = task_changes(a, b);
                if !what.is_empty() {
                    lines.push(format!("~ {} ({})", label(&new, id), what.join(", ")));
                    changes.push(json!({"task": id, "change": "edited", "what": what}));
                }
                let (sa, sb) = (old_d.status(id), new_d.status(id));
                if sa != sb {
                    lines.push(format!("  {} {}: {} -> {}", sb.glyph(), label(&new, id), sa.name(), sb.name()));
                    changes.push(json!({"task": id, "change": "status", "from": sa.name(), "to": sb.name()}));
                }
            }
            (None, None) => {}
        }
    }
    let rids: BTreeSet<&ReqId> = new.reqs.keys().chain(old.reqs.keys()).collect();
    for rid in rids {
        match (old.reqs.get(rid), new.reqs.get(rid)) {
            (None, Some(r)) => {
                lines.push(format!("+ {} {}", rid, crate::commands::req_desc(&new, r)));
                changes.push(json!({"req": rid, "change": "added", "value": r}));
            }
            (Some(r), None) => {
                lines.push(format!("- {} {}", rid, crate::commands::req_desc(&old, r)));
                changes.push(json!({"req": rid, "change": "removed", "value": r}));
            }
            (Some(a), Some(b)) if a != b => {
                lines.push(format!("~ {} {}", rid, crate::commands::req_desc(&new, b)));
                if a.from != b.from || a.on != b.on {
                    lines.push(format!("    was {}", crate::commands::req_desc(&old, a)));
                }
                changes.push(json!({"req": rid, "change": "edited", "from": a, "to": b}));
            }
            _ => {}
        }
    }
    let tids: BTreeSet<&TypeId> = new.types.keys().chain(old.types.keys()).collect();
    for tid in tids {
        match (old.types.get(tid), new.types.get(tid)) {
            (None, Some(t)) => {
                lines.push(format!("+ type {} {}", tid, t.title));
                changes.push(json!({"type": tid, "change": "added"}));
            }
            (Some(a), Some(b)) if a != b => {
                lines.push(format!("~ type {} {}", tid, b.title));
                changes.push(json!({"type": tid, "change": "edited"}));
            }
            _ => {}
        }
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "from": old_h, "to": new_h, "basis": basis,
                "events": events.iter().map(|(h, e)| json!({"hash": h, "kind": e.kind.name(), "time": e.time, "author": e.author, "message": e.message})).collect::<Vec<_>>(),
                "changes": changes,
            }))?
        );
        return Ok(());
    }
    println!("{} event(s) from {} to {}{}:", events.len(), &old_h[..10], &new_h[..10], if basis.is_empty() { String::new() } else { format!(" (since {})", basis) });
    for (h, e) in &events {
        println!("  {}  {}  {:<16} {}  -- {}", &h[..10], e.time, e.kind.name(), e.message, e.author);
    }
    if lines.is_empty() {
        println!("the plan is unchanged");
    } else {
        println!("\nchanges:");
        for l in lines {
            println!("  {}", l);
        }
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
