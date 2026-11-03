//! The markdown mirror: one page per task, plus an outline, in a folder a
//! vault (Obsidian, or any markdown tool) can browse and link to. Pages are
//! generated from the plan and never edited by hand; `tsk render` is
//! idempotent and rewrites only pages whose content changed. Backlinks find
//! the vault notes that link to a task's page.

use crate::commands::{open, say, Ctx};
use crate::model::*;
use crate::state::Status;
use anyhow::{Context as _, Result};
use serde_json::json;
use std::path::{Path, PathBuf};

const DEFAULT_DIR: &str = "tasks";

/// Where pages go, relative to the project root.
fn render_dir(c: &Ctx, dir: Option<String>) -> String {
    dir.or_else(|| c.store.config_value("render.dir")).unwrap_or_else(|| DEFAULT_DIR.to_string()).trim_matches('/').to_string()
}

fn page_name(id: &str) -> String {
    format!("{}.md", id)
}

fn link(c: &Ctx, id: &str) -> String {
    format!("[{} {}]({})", c.plan.number_of(id), c.plan.tasks[id].title, page_name(id))
}

/// A doc path relative to the project root, as a link from the render dir.
fn doc_link(rel_dir: &str, doc: &str) -> String {
    if doc.contains("://") || doc.starts_with('/') {
        return format!("[{}]({})", doc, doc);
    }
    let ups = rel_dir.split('/').filter(|s| !s.is_empty()).count();
    format!("[{}]({}{})", doc, "../".repeat(ups), doc)
}

fn checkbox(st: Status) -> &'static str {
    match st {
        Status::Todo => "[ ]",
        Status::Spike => "[?]",
        Status::InProgress => "[/]",
        Status::Paused => "[-]",
        Status::Complete => "[x]",
        Status::Dropped => "[~]",
    }
}

fn page(c: &Ctx, id: &str, rel_dir: &str) -> String {
    let t = &c.plan.tasks[id];
    let st = c.derived.status(id);
    let num = c.plan.number_of(id);
    let mut s = String::new();
    s.push_str("---\n");
    s.push_str(&format!("tsk: {}\n", id));
    s.push_str(&format!("number: \"{}\"\n", num));
    s.push_str("aliases:\n");
    s.push_str(&format!("  - \"{} {}\"\n", num, t.title.replace('"', "'")));
    s.push_str(&format!("status: {}\n", st.name()));
    for (k, v) in &t.fields {
        s.push_str(&format!("{}: \"{}\"\n", k, v.replace('"', "'")));
    }
    s.push_str("---\n");
    s.push_str(&format!("# {} {}\n\n", num, t.title));
    let mut facts = vec![format!("`{}`", id), format!("status **{}**", st.name())];
    if c.derived.is_ready(&c.plan, id) {
        facts.push("ready".into());
    }
    if c.derived.is_tentative(&c.plan, id) {
        facts.push("tentative".into());
    }
    if !t.types.is_empty() {
        facts.push(format!("type {}", t.types.join(", ")));
    }
    s.push_str(&facts.join(" · "));
    s.push('\n');
    let chain = c.plan.ancestors_and_self(id);
    if chain.len() > 1 {
        let path: Vec<String> = chain[..chain.len() - 1].iter().map(|a| link(c, &a.id)).collect();
        s.push_str(&format!("\nPath: {}\n", path.join(" › ")));
    }
    let persons: Vec<String> = c
        .plan
        .reqs_on(id)
        .iter()
        .filter_map(|r| match &r.from {
            Requirer::Person(p) => Some(format!("{}: {}", p, r.reason)),
            _ => None,
        })
        .collect();
    for p in persons {
        s.push_str(&format!("\nRequired by {}\n", p));
    }
    if !t.docs.is_empty() {
        s.push_str("\nDocs: ");
        s.push_str(&t.docs.iter().map(|d| doc_link(rel_dir, d)).collect::<Vec<_>>().join(", "));
        s.push('\n');
    }
    if !t.fields.is_empty() {
        s.push_str("\nFields: ");
        s.push_str(&t.fields.iter().map(|(k, v)| format!("`{}` = {}", k, v)).collect::<Vec<_>>().join(", "));
        s.push('\n');
    }
    if !t.body.is_empty() {
        s.push('\n');
        s.push_str(t.body.trim_end());
        s.push('\n');
    }
    let kids = c.plan.children(id);
    if !kids.is_empty() {
        s.push_str("\n## Breakdown\n\n");
        if let Some(n) = &t.narrative {
            s.push_str(n.trim_end());
            s.push_str("\n\n");
        }
        for k in kids {
            s.push_str(&format!("- {} {}\n", checkbox(c.derived.status(&k.id)), link(c, &k.id)));
        }
    }
    let from = c.plan.reqs_from(id);
    if !from.is_empty() {
        s.push_str("\n## Requires\n\n");
        for r in from {
            let k = if r.conditional { "conditional on" } else if r.blocking { "predecessor" } else { "technical, of" };
            s.push_str(&format!("- `{}` {} {}: {}\n", r.id, k, link(c, &r.on), r.reason));
        }
    }
    let on: Vec<&Requirement> = c.plan.reqs_on(id).into_iter().filter(|r| matches!(r.from, Requirer::Task(_))).collect();
    if !on.is_empty() {
        s.push_str("\n## Required by\n\n");
        for r in on {
            let k = if r.conditional { "conditional" } else if r.blocking { "successor" } else { "technical" };
            if let Requirer::Task(f) = &r.from {
                s.push_str(&format!("- `{}` {} {}: {}\n", r.id, k, link(c, f), r.reason));
            }
        }
    }
    if matches!(st, Status::Todo | Status::Spike | Status::Paused) && !c.plan.is_parent(id) {
        let blocks = c.derived.blocks(&c.plan, id);
        if !blocks.is_empty() {
            s.push_str("\n## Blocked\n\n");
            for b in blocks {
                s.push_str(&format!("- {}\n", crate::views::block_text(c, &b)));
            }
        }
    }
    let events: Vec<&(Hash, Event)> = c.history().iter().filter(|(_, e)| e.subjects.contains(&id.to_string())).collect();
    if !events.is_empty() {
        s.push_str("\n## History\n\n");
        for (h, e) in events.iter().rev() {
            s.push_str(&format!("- {} `{}` {}: {} ({}){}\n", e.time, &h[..10], e.kind.name(), e.message.replace('\n', " "), e.author, if e.forced { " **forced**" } else { "" }));
        }
    }
    s
}

fn index(c: &Ctx) -> String {
    let mut s = String::from("---\ntsk: plan\n---\n# Plan\n\n");
    s.push_str("Generated by `tsk render`; do not edit. Each line links to a task's page.\n\n");
    fn walk(c: &Ctx, id: &str, depth: usize, s: &mut String) {
        let st = c.derived.status(id);
        let mut tags = Vec::new();
        if c.derived.is_ready(&c.plan, id) {
            tags.push("ready");
        }
        if c.derived.in_loop.contains_key(id) {
            tags.push("loop");
        }
        let tag = if tags.is_empty() { String::new() } else { format!(" *{}*", tags.join(", ")) };
        s.push_str(&format!("{}- {} {}{}\n", "  ".repeat(depth), checkbox(st), link(c, id), tag));
        for k in c.plan.children(id) {
            walk(c, &k.id, depth + 1, s);
        }
    }
    for r in c.plan.roots() {
        walk(c, &r.id, 0, &mut s);
    }
    if !c.plan.types.is_empty() {
        s.push_str("\n## Types\n\n");
        for t in c.plan.types.values() {
            s.push_str(&format!("- `{}` {} ({} items)\n", t.id, t.title, t.items.len()));
        }
    }
    if let Some(h) = c.history().first() {
        s.push_str(&format!("\nAs of event `{}`, {}.\n", &h.0[..10], h.1.time));
    }
    s
}

fn write_if_changed(path: &Path, content: &str) -> Result<bool> {
    if let Ok(cur) = std::fs::read_to_string(path) {
        if cur.replace("\r\n", "\n") == content {
            return Ok(false);
        }
    }
    std::fs::write(path, content).with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

pub fn render(dir: Option<String>) -> Result<()> {
    let c = open()?;
    let rel = render_dir(&c, dir);
    let out: PathBuf = c.store.root().join(&rel);
    std::fs::create_dir_all(&out).with_context(|| format!("creating {}", out.display()))?;
    let mut changed = 0usize;
    let mut total = 0usize;
    for id in c.plan.tasks.keys() {
        total += 1;
        if write_if_changed(&out.join(page_name(id)), &page(&c, id, &rel))? {
            changed += 1;
        }
    }
    let idx = write_if_changed(&out.join("README.md"), &index(&c))?;
    if crate::commands::json_mode() {
        println!("{}", json!({"dir": out.to_string_lossy(), "pages": total, "changed": changed, "index_changed": idx}));
    } else {
        say(&format!("rendered {} page(s) into {} ({} changed{})", total, out.display(), changed, if idx { ", index updated" } else { "" }));
    }
    Ok(())
}

/// Markdown files under the project that link to the task's page, as
/// `t7.md`, `[[t7]]`, `[[t7|...]]` or `[[t7#...]]`.
pub fn backlinks(task: String, json: bool) -> Result<()> {
    let c = open()?;
    let id = c.plan.resolve(&task)?;
    let rel = render_dir(&c, None);
    let root = c.store.root();
    let needles = [format!("{}.md", id), format!("[[{}]]", id), format!("[[{}|", id), format!("[[{}#", id)];
    let mut hits: Vec<(String, usize, String)> = Vec::new();
    let skip = |name: &str| name.starts_with('.') || name == "node_modules" || name == "target";
    fn walk(dir: &Path, root: &Path, skip: &dyn Fn(&str) -> bool, render_dir: &Path, needles: &[String], hits: &mut Vec<(String, usize, String)>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if skip(&name) || p == render_dir {
                    continue;
                }
                walk(&p, root, skip, render_dir, needles, hits);
            } else if name.ends_with(".md") {
                let Ok(text) = std::fs::read_to_string(&p) else { continue };
                for (i, line) in text.lines().enumerate() {
                    if needles.iter().any(|n| line.contains(n.as_str())) {
                        let relp = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
                        hits.push((relp, i + 1, line.trim().to_string()));
                    }
                }
            }
        }
    }
    walk(&root, &root, &skip, &root.join(&rel), &needles, &mut hits);
    if json {
        println!("{}", serde_json::to_string_pretty(&hits.iter().map(|(f, l, t)| json!({"file": f, "line": l, "text": t})).collect::<Vec<_>>())?);
        return Ok(());
    }
    if hits.is_empty() {
        println!("nothing links to {} (its page is {}/{})", c.plan.label(&id), rel, page_name(&id));
        return Ok(());
    }
    for (f, l, t) in hits {
        println!("{}:{}: {}", f, l, t);
    }
    Ok(())
}
