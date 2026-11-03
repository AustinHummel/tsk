//! `tsk prune`: make the store small without losing the record. Every event
//! stays (the chain and `.tasks/log` are the permanent history); snapshots
//! older than a chosen event, and the objects only they reach, go. Reading
//! a pruned event's plan then fails with a clear message, like a shallow
//! clone in Git.

use crate::commands::{done, open_rw, say};
use crate::commit::{self, Commit};
use crate::model::*;
use anyhow::{anyhow, Result};
use serde_json::json;
use std::collections::BTreeSet;

pub fn prune(before: Option<String>, dry_run: bool) -> Result<()> {
    let c = open_rw()?;
    let head = c.store.head()?.ok_or_else(|| anyhow!("no HEAD"))?;
    let cutoff = match &before {
        Some(b) => Some(c.store.resolve_hash(b)?),
        None => None,
    };
    // Keep every event. Keep the snapshot closure of HEAD and of every event
    // at or after the cutoff; with no cutoff, of every event.
    let mut keep: BTreeSet<Hash> = BTreeSet::new();
    let mut shallow = 0usize;
    let mut past_cutoff = false;
    let mut found_cutoff = cutoff.is_none();
    for (h, ev) in c.history() {
        keep.insert(h.clone());
        if past_cutoff {
            shallow += 1;
            continue;
        }
        if c.store.has_object(&ev.snapshot) {
            c.store.snapshot_closure(&ev.snapshot, &mut keep)?;
        }
        if cutoff.as_deref() == Some(h.as_str()) {
            found_cutoff = true;
            past_cutoff = true;
        }
    }
    if !found_cutoff {
        anyhow::bail!("{} is not in this store's history", before.unwrap());
    }
    // Everything the reflog can still return to stays whole: every event on
    // those lines, with their snapshots, so `tsk reset` keeps working.
    let mut roots: BTreeSet<Hash> = BTreeSet::new();
    for e in c.store.reflog()? {
        roots.insert(e.new.clone());
        if let Some(o) = e.old {
            roots.insert(o);
        }
    }
    for r in roots {
        if !c.store.has_object(&r) || keep.contains(&r) {
            continue;
        }
        for (h, ev) in c.store.history_from(Some(r))? {
            if !keep.insert(h) {
                break;
            }
            if c.store.has_object(&ev.snapshot) {
                c.store.snapshot_closure(&ev.snapshot, &mut keep)?;
            }
        }
    }
    let _ = head;
    let mut removed = 0usize;
    let mut bytes = 0u64;
    for (h, size) in c.store.all_objects()? {
        if keep.contains(&h) {
            continue;
        }
        removed += 1;
        bytes += size;
        if !dry_run {
            c.store.remove_object(&h)?;
        }
    }
    let total: u64 = c.store.all_objects()?.iter().map(|(_, s)| s).sum();
    let msg = format!(
        "prune{}: {} object(s), {} KB{}; {} event(s) shallow; store now {} KB",
        before.as_ref().map(|b| format!(" before {}", &b[..b.len().min(10)])).unwrap_or_default(),
        removed,
        bytes / 1024,
        if dry_run { " would go" } else { " removed" },
        shallow,
        total / 1024
    );
    if dry_run {
        if crate::commands::json_mode() {
            println!("{}", json!({"dry_run": true, "objects": removed, "bytes": bytes, "shallow_events": shallow, "store_bytes": total}));
        } else {
            say(&msg);
        }
        return Ok(());
    }
    if removed == 0 && shallow == 0 {
        say("nothing to prune");
        return Ok(());
    }
    let h = commit::write(&c.store, &c.plan, Commit::new(EventKind::Pruned, msg.clone(), vec![]))?;
    done(&c.store, &h, &msg);
    Ok(())
}
