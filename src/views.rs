//! Read-only commands: the card, the chain of intent, the outline, readiness,
//! loops, review views, the log, and diffs.

use crate::color;
use crate::commands::{open, Ctx};
use crate::model::*;
use crate::plan::Plan;
use crate::state::{Block, Derived, Status};
use crate::store::Store;
use anyhow::{anyhow, bail, Context as _, Result};
use serde_json::json;
use std::collections::{BTreeSet, HashMap, HashSet};

/// `number id title`, the id dimmed when colour is on. For output only;
/// event messages use `Plan::label`.
fn clabel(c: &Ctx, id: &str) -> String {
    match c.plan.tasks.get(id) {
        Some(t) => format!("{} {} {}", c.plan.number_of(id), color::dim(id), t.title),
        None => id.to_string(),
    }
}

/// A section header in `show` and `context`.
fn hdr(s: &str) -> String {
    color::bold(s)
}

fn hash10(h: &str) -> String {
    color::dim(&h[..10])
}

/// The leaves under a task (not the task itself), by what they are.
#[derive(Default)]
struct Counts {
    open: usize,
    ready: usize,
    done: usize,
    dropped: usize,
}

impl Counts {
    /// `(7 open, 2 ready, 3 done)`, zero parts left out; with `finished_only`
    /// just the done and dropped parts (what `plan` hides).
    fn text(&self, finished_only: bool) -> String {
        let mut parts = Vec::new();
        if !finished_only {
            if self.open > 0 {
                parts.push(format!("{} open", self.open));
            }
            if self.ready > 0 {
                parts.push(format!("{} ready", self.ready));
            }
        }
        if self.done > 0 {
            parts.push(format!("{} done", self.done));
        }
        if self.dropped > 0 {
            parts.push(format!("{} dropped", self.dropped));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!(" ({})", parts.join(", "))
        }
    }
    fn json(&self) -> serde_json::Value {
        json!({"open": self.open, "ready": self.ready, "done": self.done, "dropped": self.dropped})
    }
}

fn leaf_counts(c: &Ctx, id: &str, filters: &[(String, Option<String>)]) -> Counts {
    let mut k = Counts::default();
    for d in c.plan.subtree(id) {
        if d == id || c.plan.is_parent(&d) || (!filters.is_empty() && !c.plan.matches(&d, filters)) {
            continue;
        }
        match c.derived.status(&d) {
            Status::Complete => k.done += 1,
            Status::Dropped => k.dropped += 1,
            _ => {
                k.open += 1;
                if c.derived.is_ready(&c.plan, &d) {
                    k.ready += 1;
                }
            }
        }
    }
    k
}

fn tags_of(c: &Ctx, id: &str) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    if c.derived.is_ready(&c.plan, id) {
        tags.push("ready".into());
    }
    if c.derived.in_loop.contains_key(id) {
        tags.push("loop".into());
    }
    if c.derived.is_tentative(&c.plan, id) {
        tags.push("tentative".into());
    }
    if c.plan.tasks.get(id).map(|t| t.ongoing).unwrap_or(false) {
        tags.push("ongoing".into());
    }
    if c.derived.is_overdue(&c.plan, id) {
        tags.push("overdue".into());
    }
    // The gating date, own or inherited; only on leaves, since a parent is
    // never ready anyway.
    if !c.plan.is_parent(id) && matches!(c.derived.status(id), Status::Todo | Status::Spike | Status::Paused) {
        if let Some((_, d)) = c.derived.not_before(&c.plan, id) {
            tags.push(format!("not before {}", d));
        }
    }
    tags
}

/// The one line `--scope` prints under a task: its fit, else the first
/// non-empty line of its narrative (a parent) or body (a leaf), cut short.
fn scope_line(c: &Ctx, id: &str) -> Option<String> {
    let t = &c.plan.tasks[id];
    let text = t
        .fit
        .as_deref()
        .filter(|f| !f.trim().is_empty())
        .or_else(|| if c.plan.is_parent(id) || t.ongoing { t.narrative.as_deref() } else { None })
        .unwrap_or(&t.body);
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let cut: String = line.chars().take(100).collect();
    Some(if cut.len() < line.len() { format!("{}...", cut.trim_end()) } else { cut })
}

pub(crate) fn block_text(c: &Ctx, b: &Block) -> String {
    match b {
        Block::NotBefore(t, d) => {
            if c.plan.is_parent(t) || c.plan.tasks.get(t).map(|x| x.ongoing).unwrap_or(false) {
                format!("not before {} (after, on {})", d, c.plan.number_of(t))
            } else {
                format!("not before {}", d)
            }
        }
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
            "closed": c.derived.is_closed(&c.plan, &c.leaf, &id),
            "overdue": c.derived.is_overdue(&c.plan, &id),
            "not_before": c.derived.not_before(&c.plan, &id).map(|(t, d)| json!({"on": t, "after": d})),
            "path": c.plan.ancestors_and_self(&id).iter().map(|a| a.id.clone()).collect::<Vec<_>>(),
            "children": c.plan.children(&id).iter().map(|k| json!({"id": k.id, "number": c.plan.number_of(&k.id), "title": k.title, "status": c.derived.status(&k.id).name(), "fit": k.fit, "ongoing": k.ongoing})).collect::<Vec<_>>(),
            "required_by": c.plan.reqs_on(&id),
            "requires": c.plan.reqs_from(&id),
            "blocks": blocks.iter().map(|b| block_text(&c, b)).collect::<Vec<_>>(),
            "events": events.iter().map(|(h, e)| json!({"hash": h, "kind": e.kind.name(), "time": e.time, "author": e.author, "message": e.message, "forced": e.forced})).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!("{} {}  {}", color::glyph(st), c.plan.number_of(&id), t.title);
    let mut facts = vec![format!("id {}", color::dim(&id)), format!("status {}", st.name())];
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
    if t.ongoing {
        facts.push(if c.derived.is_closed(&c.plan, &c.leaf, &id) { "ongoing, closed".to_string() } else { "ongoing".to_string() });
    }
    if c.derived.is_tentative(&c.plan, &id) {
        facts.push("tentative".to_string());
    }
    if let Some(p) = &t.priority {
        facts.push(format!("priority {}", p));
    }
    if let Some(a) = &t.after {
        facts.push(format!("after {}", a));
    }
    if let Some(d) = &t.due {
        facts.push(format!("due {}{}", d, if c.derived.is_overdue(&c.plan, &id) { " (overdue)" } else { "" }));
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
        println!("\n{}", hdr("path:"));
        for a in &path[..path.len() - 1] {
            println!("  {} {}{}", c.plan.number_of(&a.id), a.title, if a.ongoing { " (ongoing)" } else { "" });
        }
        if let Some(f) = &t.fit {
            println!("  {} {}", hdr("fits:"), f);
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
        println!("\n{}", hdr("required by:"));
        for r in on {
            let k = match &r.from {
                Requirer::Person(_) => "by",
                _ if r.conditional => "conditional",
                _ if r.blocking => "successor",
                _ => "technical",
            };
            println!("  {} {} [{}]: {}{}", r.id, k, c.plan.requirer_label(&r.from), r.reason, req_fields(r));
        }
    }
    let from = c.plan.reqs_from(&id);
    if !from.is_empty() {
        println!("\n{}", hdr("requires:"));
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
        println!("\n{}", hdr("inherited:"));
        for l in inherited {
            println!("{}", l);
        }
    }
    let kids = c.plan.children(&id);
    if !kids.is_empty() || (t.ongoing && t.narrative.is_some()) {
        println!("\n{}", hdr(if t.ongoing { "admits:" } else { "breakdown:" }));
        if let Some(n) = &t.narrative {
            for l in n.lines() {
                println!("  {}", l);
            }
        }
        for k in kids {
            println!("  {} {}{}", color::glyph(c.derived.status(&k.id)), clabel(&c, &k.id), if k.ongoing { "  <ongoing>" } else { "" });
            if let Some(f) = &k.fit {
                println!("      {}", f);
            }
        }
    } else if !blocks.is_empty() && st != Status::Complete && st != Status::Dropped {
        println!("\n{}", hdr("blocked:"));
        for b in &blocks {
            println!("  {}", block_text(&c, b));
        }
    } else if c.derived.is_ready(&c.plan, &id) {
        println!("\n{}", color::green("ready to start"));
    }
    if !events.is_empty() {
        println!("\n{}", hdr("history:"));
        for (h, e) in events.iter().take(8) {
            println!("  {}  {}  {}{}", hash10(h), e.time, e.kind.name(), if e.forced { color::red(" (forced)") } else { String::new() });
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
                "fit": a.fit, "ongoing": a.ongoing,
                "status": c.derived.status(&a.id).name(),
            })).collect::<Vec<_>>(),
            "requires": c.plan.reqs_from(&id),
            "required_by": c.plan.reqs_on(&id),
            "inherited": inherited.iter().map(|(a, r)| json!({"via": a.id, "req": r})).collect::<Vec<_>>(),
            "inherited_required_by": inherited_on.iter().map(|(a, r)| json!({"via": a.id, "req": r})).collect::<Vec<_>>(),
            "siblings": siblings.iter().map(|s| json!({"id": s.id, "number": c.plan.number_of(&s.id), "title": s.title, "status": c.derived.status(&s.id).name(), "fit": s.fit})).collect::<Vec<_>>(),
            "children": kids.iter().map(|k| json!({"id": k.id, "number": c.plan.number_of(&k.id), "title": k.title, "status": c.derived.status(&k.id).name(), "fit": k.fit})).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!("context for {} {}  {}", c.plan.number_of(&id), color::dim(&id), t.title);
    let mut facts = vec![format!("status {}", st.name())];
    if c.derived.is_ready(&c.plan, &id) {
        facts.push(color::green("ready"));
    }
    if c.derived.is_tentative(&c.plan, &id) {
        facts.push(color::yellow("tentative"));
    }
    if t.ongoing {
        facts.push(color::magenta("ongoing"));
    }
    if let Some(a) = &t.after {
        facts.push(format!("after {}", a));
    }
    if let Some(d) = &t.due {
        facts.push(format!("due {}", d));
    }
    println!("    {}", facts.join(" · "));
    println!("\n{}", hdr("chain of intent, root first:"));
    for (depth, a) in chain.iter().enumerate() {
        let pad = "  ".repeat(depth + 1);
        let here = if a.id == id { "  <- this task" } else { "" };
        println!("{}{} {} {}{}{}", pad, color::glyph(c.derived.status(&a.id)), c.plan.number_of(&a.id), a.title, if a.ongoing { " (ongoing)" } else { "" }, here);
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
                println!("{}  {}", pad, if a.ongoing { "admits:" } else { "narrative:" });
                for l in n.lines().filter(|l| !l.trim().is_empty()) {
                    println!("{}    {}", pad, l);
                }
            }
            // How the next level down sits in this one.
            if let Some(next) = chain.get(depth + 1) {
                if let Some(f) = &next.fit {
                    println!("{}  fits {}: {}", pad, c.plan.number_of(&a.id), f);
                }
            }
        }
    }
    if !t.body.is_empty() || t.fit.is_some() {
        println!("\n{}", hdr("card:"));
        if let Some(f) = &t.fit {
            println!("  fits: {}", f);
        }
        for l in t.body.lines() {
            println!("  {}", l);
        }
    }
    if let Some(n) = &t.narrative {
        println!("\n{}", hdr(if t.ongoing { "admits:" } else { "narrative:" }));
        for l in n.lines() {
            println!("  {}", l);
        }
    }
    let from = c.plan.reqs_from(&id);
    if !from.is_empty() || !inherited.is_empty() {
        println!("\n{}", hdr("this task requires:"));
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
        println!("\n{}", hdr("required of this task:"));
        for r in on {
            let k = if r.conditional { "conditional" } else if r.blocking { "successor" } else { "technical" };
            println!("  {} {} [{}]: {}", r.id, k, c.plan.requirer_label(&r.from), r.reason);
        }
        for (a, r) in &inherited_on {
            println!("  {} via {}: successor [{}]: {}", r.id, c.plan.number_of(&a.id), c.plan.requirer_label(&r.from), r.reason);
        }
    }
    if !blocks.is_empty() && matches!(st, Status::Todo | Status::Spike | Status::Paused) {
        println!("\n{}", hdr("blocked:"));
        for b in &blocks {
            println!("  {}", block_text(&c, b));
        }
    }
    if !siblings.is_empty() {
        println!("\n{}", hdr("siblings:"));
        for s in siblings {
            println!("  {} {} {}", color::glyph(c.derived.status(&s.id)), c.plan.number_of(&s.id), s.title);
            if let Some(f) = &s.fit {
                println!("      {}", f);
            }
        }
    }
    if !kids.is_empty() {
        println!("\n{}", hdr("children:"));
        for k in kids {
            println!("  {} {} {}", color::glyph(c.derived.status(&k.id)), c.plan.number_of(&k.id), k.title);
            if let Some(f) = &k.fit {
                println!("      {}", f);
            }
        }
    }
    Ok(())
}

/// One task's line in `plan` and `ls`: glyph, number, id, title, then a
/// count of the leaves under it when one is wanted, then the tags.
fn outline_line(c: &Ctx, id: &str, st: Status, counts: Option<(&Counts, bool)>, tags: &[String]) -> String {
    let marker = counts.map(|(k, finished_only)| k.text(finished_only)).unwrap_or_default();
    let refs: Vec<&str> = tags.iter().map(String::as_str).collect();
    format!("{} {}{}{}", color::glyph(st), clabel(c, id), marker, color::tags(&refs))
}

/// What else attaches to a task that has no children: the open tasks that
/// carry each of its field values, and the requirements on it. Printed
/// under `plan <task>` so a one-line answer is never the whole story.
fn childless_hints(c: &Ctx, id: &str) -> Vec<String> {
    let t = &c.plan.tasks[id];
    let mut hints = Vec::new();
    for (k, v) in &t.fields {
        let filter = [(k.clone(), Some(v.clone()))];
        let n = c
            .plan
            .tasks
            .keys()
            .filter(|o| *o != id && !matches!(c.derived.status(o), Status::Complete | Status::Dropped) && c.plan.matches(o, &filter))
            .count();
        if n > 0 {
            hints.push(format!("{} open task{} carr{} {}={}: tsk plan --where {}={}", n, if n == 1 { "" } else { "s" }, if n == 1 { "ies" } else { "y" }, k, v, k, v));
        }
    }
    let on = c.plan.reqs_on(id).len();
    let from = c.plan.reqs_from(id).len();
    if on + from > 0 {
        let mut s = String::new();
        if on > 0 {
            s.push_str(&format!("{} requirement{} on it", on, if on == 1 { "" } else { "s" }));
        }
        if from > 0 {
            if !s.is_empty() {
                s.push_str(", ");
            }
            s.push_str(&format!("{} it places", from));
        }
        hints.push(format!("{}: tsk show {}", s, c.plan.number_of(id)));
    }
    hints
}

pub fn plan(task: Option<String>, all: bool, filters: Vec<String>, depth: Option<usize>, scope: bool, json: bool) -> Result<()> {
    let c = open()?;
    let filters = parse_filters(&filters);
    let roots: Vec<TaskId> = match &task {
        Some(t) => vec![c.plan.resolve(t)?],
        None => c.plan.roots().iter().map(|t| t.id.clone()).collect(),
    };
    #[allow(clippy::too_many_arguments)]
    fn walk(c: &Ctx, id: &TaskId, depth: usize, all: bool, filters: &[(String, Option<String>)], max: Option<usize>, scope: bool, out: &mut Vec<serde_json::Value>, text: &mut String) {
        let st = c.derived.status(id);
        if !all && matches!(st, Status::Complete | Status::Dropped) {
            return;
        }
        if !filters.is_empty() && !c.plan.subtree(id).iter().any(|d| c.plan.matches(d, filters)) {
            return;
        }
        let t = &c.plan.tasks[id];
        let tags = tags_of(c, id);
        let kids = c.plan.children(id);
        let cut = !kids.is_empty() && max.map(|m| depth >= m).unwrap_or(false);
        let counts = if kids.is_empty() { None } else { Some(leaf_counts(c, id, filters)) };
        // At a depth cut the whole count; otherwise only what `plan` hides,
        // the finished leaves, and nothing with `--all`.
        let marker = match &counts {
            Some(k) if cut => Some((k, false)),
            Some(k) if !all => Some((k, true)),
            _ => None,
        };
        text.push_str(&format!("{}{}\n", "    ".repeat(depth), outline_line(c, id, st, marker, &tags)));
        let mut row = json!({"id": id, "number": c.plan.number_of(id), "title": t.title, "status": st.name(), "depth": depth, "tags": tags, "fields": t.fields});
        if let Some(k) = &counts {
            row["counts"] = k.json();
        }
        if scope {
            let line = scope_line(c, id);
            if let Some(l) = &line {
                text.push_str(&format!("{}    {}\n", "    ".repeat(depth), color::dim(l)));
            }
            row["scope"] = json!(line);
        }
        out.push(row);
        if cut {
            return;
        }
        for k in kids {
            walk(c, &k.id, depth + 1, all, filters, max, scope, out, text);
        }
    }
    let mut out = Vec::new();
    let mut text = String::new();
    for r in &roots {
        walk(&c, r, 0, all, &filters, depth, scope, &mut out, &mut text);
    }
    if let (Some(_), 1) = (&task, out.len()) {
        let id = &roots[0];
        if c.plan.children(id).is_empty() {
            let hints = childless_hints(&c, id);
            if !hints.is_empty() {
                text.push_str("    no children\n");
                for h in hints {
                    text.push_str(&format!("    {}\n", h));
                }
            }
        }
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

/// One level, flat: the roots, or a task's children, each with the count of
/// the leaves under it.
pub fn ls(task: Option<String>, all: bool, filters: Vec<String>, scope: bool, json: bool) -> Result<()> {
    let c = open()?;
    let filters = parse_filters(&filters);
    let (items, under): (Vec<TaskId>, Option<TaskId>) = match task {
        Some(t) => {
            let id = c.plan.resolve(&t)?;
            (c.plan.children(&id).iter().map(|k| k.id.clone()).collect(), Some(id))
        }
        None => (c.plan.roots().iter().map(|t| t.id.clone()).collect(), None),
    };
    let mut out = Vec::new();
    let mut text = String::new();
    for id in &items {
        let st = c.derived.status(id);
        if !all && matches!(st, Status::Complete | Status::Dropped) {
            continue;
        }
        if !filters.is_empty() && !c.plan.subtree(id).iter().any(|d| c.plan.matches(d, &filters)) {
            continue;
        }
        let t = &c.plan.tasks[id];
        let tags = tags_of(&c, id);
        let counts = if c.plan.children(id).is_empty() { None } else { Some(leaf_counts(&c, id, &filters)) };
        text.push_str(&format!("{}\n", outline_line(&c, id, st, counts.as_ref().map(|k| (k, false)), &tags)));
        let mut row = json!({"id": id, "number": c.plan.number_of(id), "title": t.title, "status": st.name(), "tags": tags, "fields": t.fields});
        if let Some(k) = &counts {
            row["counts"] = k.json();
        }
        if scope {
            let line = scope_line(&c, id);
            if let Some(l) = &line {
                text.push_str(&format!("    {}\n", color::dim(l)));
            }
            row["scope"] = json!(line);
        }
        out.push(row);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else if text.is_empty() {
        match under {
            Some(id) if items.is_empty() => println!("{} has no children", c.plan.label(&id)),
            Some(id) => {
                let k = leaf_counts(&c, &id, &filters);
                println!("(no open children under {}{}; `--all` shows them)", c.plan.label(&id), k.text(true));
            }
            None => println!("(no open tasks)"),
        }
    } else {
        print!("{}", text);
    }
    Ok(())
}

/// The direct successors of a task and of its ancestors: what it holds up.
fn holds_up(c: &Ctx, id: &str) -> Vec<TaskId> {
    let mut v: Vec<TaskId> = Vec::new();
    for a in c.plan.ancestors_and_self(id) {
        for r in c.plan.reqs_on(&a.id) {
            if let (true, Requirer::Task(f)) = (r.blocking, &r.from) {
                if !v.contains(f) && !matches!(c.derived.status(f), Status::Complete | Status::Dropped) {
                    v.push(f.clone());
                }
            }
        }
    }
    v.sort_by_key(|t| c.plan.tasks[t].seq);
    v
}

pub fn ready(blocked: bool, filters: Vec<String>, holds: bool, json: bool) -> Result<()> {
    let c = open()?;
    let filters = parse_filters(&filters);
    let ready: Vec<TaskId> = c.derived.ready(&c.plan).into_iter().filter(|t| c.plan.matches(t, &filters)).collect();
    // Open leaves a date is holding back, soonest first: the calendar an
    // orient must see.
    let mut not_before: Vec<(TaskId, TaskId, String)> = c
        .plan
        .tasks
        .values()
        .filter(|t| !c.plan.is_parent(&t.id) && !t.ongoing && matches!(c.derived.status(&t.id), Status::Todo | Status::Spike | Status::Paused) && c.plan.matches(&t.id, &filters))
        .filter_map(|t| c.derived.not_before(&c.plan, &t.id).map(|(on, d)| (t.id.clone(), on, d)))
        .collect();
    not_before.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| c.plan.tasks[&a.0].seq.cmp(&c.plan.tasks[&b.0].seq)));
    // Ongoing tasks with something finished under them and nothing open:
    // the next occurrence may be missing.
    let mut empty_ongoing: Vec<TaskId> = c
        .plan
        .tasks
        .values()
        .filter(|t| t.ongoing && !matches!(c.derived.status(&t.id), Status::Complete | Status::Dropped) && c.plan.matches(&t.id, &filters))
        .filter(|t| c.plan.children(&t.id).iter().any(|k| c.derived.status(&k.id) == Status::Complete) && c.derived.open_under(&c.plan, &t.id).is_empty())
        .map(|t| t.id.clone())
        .collect();
    empty_ongoing.sort_by_key(|t| c.plan.tasks[t].seq);
    let due_text = |t: &TaskId| -> String {
        match &c.plan.tasks[t].due {
            Some(d) => format!("  due {}{}", d, if c.derived.is_overdue(&c.plan, t) { color::red(" (overdue)") } else { String::new() }),
            None => String::new(),
        }
    };
    // Leaves in progress, and who started each (the latest `started` event
    // naming it), so a session sees what it has on its hands.
    let me = c.store.author();
    let mut in_progress: Vec<TaskId> = c.plan.tasks.values().filter(|t| !c.plan.is_parent(&t.id) && !t.ongoing && c.derived.status(&t.id) == Status::InProgress && c.plan.matches(&t.id, &filters)).map(|t| t.id.clone()).collect();
    in_progress.sort_by_key(|t| c.plan.tasks[t].seq);
    let mut started_by: HashMap<TaskId, String> = HashMap::new();
    if !in_progress.is_empty() {
        let pending: HashSet<&TaskId> = in_progress.iter().collect();
        for (_, e) in c.history() {
            if e.kind == EventKind::Started {
                for s in &e.subjects {
                    if pending.contains(s) && !started_by.contains_key(s) {
                        started_by.insert(s.clone(), e.author.clone());
                    }
                }
            }
            if started_by.len() == pending.len() {
                break;
            }
        }
    }
    let mine: Vec<&TaskId> = in_progress.iter().filter(|t| started_by.get(*t) == Some(&me)).collect();
    let mut blocked_list: Vec<(TaskId, Vec<Block>)> = Vec::new();
    if blocked || json {
        let mut leaves: Vec<&Task> = c.plan.tasks.values().filter(|t| !c.plan.is_parent(&t.id) && !t.ongoing).collect();
        leaves.sort_by_key(|t| t.seq);
        for t in leaves {
            let st = c.derived.status(&t.id);
            // What a date alone holds back has its own section.
            if matches!(st, Status::Todo | Status::Spike | Status::Paused) && !c.derived.is_ready(&c.plan, &t.id) && c.plan.matches(&t.id, &filters) && !not_before.iter().any(|(id, _, _)| *id == t.id) {
                blocked_list.push((t.id.clone(), c.derived.blocks(&c.plan, &t.id)));
            }
        }
    }
    if json {
        let v = json!({
            "author": me,
            "in_progress": in_progress.iter().map(|t| json!({"id": t, "number": c.plan.number_of(t), "title": c.plan.tasks[t].title, "author": started_by.get(t), "mine": started_by.get(t) == Some(&me), "fields": c.plan.tasks[t].fields})).collect::<Vec<_>>(),
            "ready": ready.iter().map(|t| {
                let mut row = json!({"id": t, "number": c.plan.number_of(t), "title": c.plan.tasks[t].title, "status": c.derived.status(t).name(), "fields": c.plan.tasks[t].fields, "due": c.plan.tasks[t].due, "overdue": c.derived.is_overdue(&c.plan, t)});
                if holds {
                    row["holds"] = json!(holds_up(&c, t));
                }
                row
            }).collect::<Vec<_>>(),
            "not_before": not_before.iter().map(|(t, on, d)| json!({"id": t, "number": c.plan.number_of(t), "title": c.plan.tasks[t].title, "after": d, "on": on, "due": c.plan.tasks[t].due})).collect::<Vec<_>>(),
            "empty_ongoing": empty_ongoing,
            "blocked": blocked_list.iter().map(|(t, bs)| json!({"id": t, "number": c.plan.number_of(t), "title": c.plan.tasks[t].title, "blocks": bs.iter().map(|b| block_text(&c, b)).collect::<Vec<_>>()})).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    if !mine.is_empty() {
        println!("in progress (you):");
        for t in &mine {
            println!("  {} {}{}", color::glyph(Status::InProgress), clabel(&c, t), due_text(t));
        }
    }
    if ready.is_empty() {
        println!("nothing is ready");
    } else {
        println!("ready:");
        for t in &ready {
            println!("  {} {}{}", color::glyph(c.derived.status(t)), clabel(&c, t), due_text(t));
            if holds {
                let h = holds_up(&c, t);
                if h.is_empty() {
                    println!("      holds up nothing");
                } else {
                    println!("      holds up: {}", h.iter().map(|x| c.plan.number_of(x)).collect::<Vec<_>>().join(", "));
                }
            }
        }
    }
    if !not_before.is_empty() {
        println!("not before:");
        for (t, on, d) in &not_before {
            let via = if *on == *t { String::new() } else { format!(" (after, on {})", c.plan.number_of(on)) };
            let due = c.plan.tasks[t].due.as_ref().map(|x| format!(", due {}", x)).unwrap_or_default();
            println!("  {} {}  not before {}{}{}", color::glyph(c.derived.status(t)), clabel(&c, t), d, via, due);
        }
    }
    if blocked {
        if blocked_list.is_empty() {
            println!("nothing is blocked");
        } else {
            println!("blocked:");
            for (t, bs) in &blocked_list {
                println!("  {} {}", color::glyph(c.derived.status(t)), clabel(&c, t));
                for b in bs {
                    println!("      {}", block_text(&c, b));
                }
            }
        }
    }
    if !empty_ongoing.is_empty() {
        println!("ongoing, nothing open under it: {}", empty_ongoing.iter().map(|t| clabel(&c, t)).collect::<Vec<_>>().join("; "));
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
        println!("{}", color::red(&format!("loop #{}:", i + 1)));
        for t in &l.tasks {
            println!("  {} {}", color::glyph(c.derived.status(t)), clabel(&c, t));
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
    // Live ongoing tasks: how many piles exist and how fast each grows (the
    // date of its newest scope-adding event).
    let mut ongoing: Vec<(TaskId, usize, String)> = Vec::new();
    let mut piles: Vec<&Task> = c.plan.tasks.values().filter(|t| t.ongoing && !matches!(c.derived.status(&t.id), Status::Complete | Status::Dropped)).collect();
    piles.sort_by_key(|t| t.seq);
    for t in piles {
        let last = c
            .history()
            .iter()
            .find(|(_, e)| matches!(e.kind, EventKind::BrokenDown | EventKind::Amended | EventKind::Adopted) && e.subjects.first() == Some(&t.id))
            .map(|(_, e)| e.time[..10].to_string())
            .unwrap_or_else(|| "never".to_string());
        ongoing.push((t.id.clone(), c.derived.open_under(&c.plan, &t.id).len(), last));
    }
    // Pieces a split named but whose card was not written: a core child
    // (no type on the host) with the host's own body and the generated fit.
    let mut unscoped: Vec<TaskId> = c
        .plan
        .tasks
        .values()
        .filter(|t| !matches!(c.derived.status(&t.id), Status::Complete | Status::Dropped))
        .filter(|t| match (&t.core_of, c.plan.tasks.get(t.core_of.as_deref().unwrap_or(""))) {
            (Some(_), Some(host)) => host.types.is_empty() && (t.body == host.body) && (t.fit.as_deref().map(|f| f.starts_with("what ") && f.contains(" was before ")).unwrap_or(false) || t.title.ends_with("(core)")),
            _ => false,
        })
        .map(|t| t.id.clone())
        .collect();
    unscoped.sort_by_key(|t| c.plan.tasks[t].seq);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "required": rows.iter().map(|(d, p, r, t)| json!({"date": d, "person": p, "reason": r, "task": t})).collect::<Vec<_>>(),
                "unowned_roots": unowned,
                "ongoing": ongoing.iter().map(|(t, n, d)| json!({"task": t, "open_under": n, "last_scope_added": d})).collect::<Vec<_>>(),
                "unscoped_pieces": unscoped,
            }))?
        );
        return Ok(());
    }
    if rows.is_empty() && unowned.is_empty() && ongoing.is_empty() && unscoped.is_empty() {
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
    if !ongoing.is_empty() {
        println!("ongoing:");
        for (t, n, d) in &ongoing {
            println!("  {}  {} open under it, last scope added {}", c.plan.label(t), n, d);
        }
    }
    if !unscoped.is_empty() {
        println!("pieces still carrying the parent's card:");
        for t in &unscoped {
            println!("  {}", c.plan.label(t));
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
        println!("  {}  {}  {}  {}", hash10(h), e.time, e.kind.name(), e.message);
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
        let forced = if e.forced { format!("  {}", color::red("FORCED")) } else { String::new() };
        println!("{}  {}  {:<16} {}{}{}  -- {}", hash10(h), e.time, e.kind.name(), e.message, subj, forced, e.author);
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
                "title: {}\nparent: {}\nfit: {}\nongoing: {}\nafter: {}\ndue: {}\ntypes: {}\ndocs: {}\nfields: {}\nnarrative: {}\n---\n{}\n",
                t.title,
                t.parent.clone().unwrap_or_default(),
                t.fit.clone().unwrap_or_default(),
                t.ongoing,
                t.after.clone().unwrap_or_default(),
                t.due.clone().unwrap_or_default(),
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
    let derived = Derived::compute(&plan, &leaf, crate::clock::now()?);
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
    if a.fit != b.fit {
        v.push("fit");
    }
    if a.ongoing != b.ongoing {
        v.push("ongoing");
    }
    if a.after != b.after || a.due != b.due {
        v.push("dates");
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
    if a.priority != b.priority {
        v.push("priority");
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
                    lines.push(format!("  {} {}: {} -> {}", color::glyph(sb), label(&new, id), sa.name(), sb.name()));
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
    println!("{} event(s) from {} to {}{}:", events.len(), hash10(old_h), hash10(new_h), if basis.is_empty() { String::new() } else { format!(" (since {})", basis) });
    for (h, e) in &events {
        println!("  {}  {}  {:<16} {}  -- {}", hash10(h), e.time, e.kind.name(), e.message, e.author);
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
