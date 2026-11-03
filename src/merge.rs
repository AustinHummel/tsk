//! `tsk merge`: bring another copy of this store back into one line of
//! history. Two copies share every event up to the point they diverged
//! (same hashes), and git carries the objects add-only, so a merge needs
//! only the other HEAD.
//!
//! The merge is canonical: both sides' events since the fork are replayed
//! from the fork point in one order, chronological (ties by the original
//! event's hash), each as a patch (its own snapshot diffed against its
//! parent's). Replayed from either copy, the result is the same chain of
//! events, the same ids and the same snapshots, so two copies that merge
//! each other converge instead of disagreeing forever about which task is
//! `t20`. Ids that collide are renumbered in that order and the map is
//! reported; a field changed differently on both sides is a conflict,
//! resolved only by `--prefer`; a replayed event names its original in
//! `replayed_from`, which is how a copy knows what it already holds.

use crate::commands::{done, json_mode, say, store_open};
use crate::commit::{self, Commit};
use crate::model::*;
use crate::plan::Plan;
use crate::store::{Lock, Store};
use anyhow::{anyhow, bail, Context as _, Result};
use serde_json::json;
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Side {
    Ours,
    Theirs,
}

impl Side {
    fn name(self) -> &'static str {
        match self {
            Side::Ours => "ours",
            Side::Theirs => "theirs",
        }
    }
    fn parse(s: Option<String>) -> Result<Option<Side>> {
        Ok(match s.as_deref() {
            None => None,
            Some("ours") => Some(Side::Ours),
            Some("theirs") => Some(Side::Theirs),
            Some(x) => bail!("--prefer takes `ours` or `theirs`, not `{}`", x),
        })
    }
}

/// What a merge did, or would do.
#[derive(Default)]
pub struct Outcome {
    /// (original event, event on the merged line); empty hashes on a dry run.
    pub replayed: Vec<(Hash, Hash)>,
    /// How many of our own events were rewritten to take their place in
    /// chronological order.
    pub ours_rewritten: usize,
    /// (side, old id, new id) for tasks and requirements renumbered.
    pub renumbered: Vec<(Side, String, String)>,
    pub conflicts: Vec<String>,
    pub resolved: usize,
    pub both_touched: Vec<String>,
    pub fast_forward: bool,
    pub head: Option<Hash>,
}

/// One event to replay: its identity (the original event it stands for),
/// its hash on the chain it came from, and which side that was.
struct Item {
    id: Hash,
    hash: Hash,
    ev: Event,
    side: Side,
}

/// The replay state: the merged plan so far and the id maps, per side.
struct Replay {
    plan: Plan,
    side: Side,
    tmap: HashMap<(Side, TaskId), TaskId>,
    rmap: HashMap<(Side, ReqId), ReqId>,
    prefer: Option<Side>,
    conflicts: Vec<String>,
    resolved: usize,
    renumbered: Vec<(Side, String, String)>,
}

impl Replay {
    fn t(&self, id: &str) -> TaskId {
        self.tmap.get(&(self.side, id.to_string())).cloned().unwrap_or_else(|| id.to_string())
    }
    fn r(&self, id: &str) -> ReqId {
        self.rmap.get(&(self.side, id.to_string())).cloned().unwrap_or_else(|| id.to_string())
    }
    fn requirer(&self, q: &Requirer) -> Requirer {
        match q {
            Requirer::Task(t) => Requirer::Task(self.t(t)),
            p => p.clone(),
        }
    }
    /// Whether a change from the side being replayed wins a conflict.
    fn wins(&self) -> bool {
        self.prefer == Some(self.side)
    }

    /// Three-way rule for one field: take the change when the current
    /// value is untouched (or already agrees); otherwise a conflict, unless
    /// `--prefer` says which side wins.
    fn field<T: PartialEq + Clone>(&mut self, what: &str, cur: &mut T, pre: &T, post: &T) {
        if pre == post {
            return;
        }
        if *cur == *pre || *cur == *post {
            *cur = post.clone();
            return;
        }
        match self.prefer {
            Some(_) => {
                if self.wins() {
                    *cur = post.clone();
                }
                self.resolved += 1;
            }
            None => self.conflicts.push(format!("{} ({} vs {})", what, Side::Ours.name(), Side::Theirs.name())),
        }
    }

    /// Set-like lists (docs, types): apply additions and removals.
    fn list(&mut self, cur: &mut Vec<String>, pre: &[String], post: &[String]) {
        for x in post {
            if !pre.contains(x) && !cur.contains(x) {
                cur.push(x.clone());
            }
        }
        for x in pre {
            if !post.contains(x) {
                cur.retain(|c| c != x);
            }
        }
    }

    fn fields(&mut self, what: &str, cur: &mut Fields, pre: &Fields, post: &Fields) {
        let keys: BTreeSet<&String> = pre.keys().chain(post.keys()).collect();
        for k in keys {
            let mut c = cur.get(k).cloned();
            self.field(&format!("{} field {}", what, k), &mut c, &pre.get(k).cloned(), &post.get(k).cloned());
            match c {
                Some(v) => {
                    cur.insert(k.clone(), v);
                }
                None => {
                    cur.remove(k);
                }
            }
        }
    }

    fn task_added(&mut self, t: &Task) {
        let mut nt = t.clone();
        let num: u64 = t.id.trim_start_matches('t').parse().unwrap_or(0);
        if self.plan.tasks.contains_key(&t.id) {
            nt.id = self.plan.new_task_id();
            nt.seq = self.plan.next_task;
            self.tmap.insert((self.side, t.id.clone()), nt.id.clone());
            self.renumbered.push((self.side, t.id.clone(), nt.id.clone()));
        } else if num > self.plan.next_task {
            self.plan.next_task = num;
        }
        nt.parent = t.parent.as_deref().map(|p| self.t(p));
        nt.core_of = t.core_of.as_deref().map(|p| self.t(p));
        self.plan.tasks.insert(nt.id.clone(), nt);
    }

    fn task_changed(&mut self, pre: &Task, post: &Task, label: &str) {
        let id = self.t(&pre.id);
        let Some(mut cur) = self.plan.tasks.get(&id).cloned() else {
            self.conflicts.push(format!("{}: changed but missing on the merged line", label));
            return;
        };
        let w = |f: &str| format!("{} {}", label, f);
        self.field(&w("title"), &mut cur.title, &pre.title, &post.title);
        let (pp, pq) = (pre.parent.as_deref().map(|p| self.t(p)), post.parent.as_deref().map(|p| self.t(p)));
        self.field(&w("parent"), &mut cur.parent, &pp, &pq);
        self.field(&w("body"), &mut cur.body, &pre.body, &post.body);
        self.field(&w("narrative"), &mut cur.narrative, &pre.narrative, &post.narrative);
        self.field(&w("priority"), &mut cur.priority, &pre.priority, &post.priority);
        self.field(&w("due"), &mut cur.due, &pre.due, &post.due);
        self.field(&w("from_item"), &mut cur.from_item, &pre.from_item, &post.from_item);
        self.list(&mut cur.types, &pre.types, &post.types);
        self.list(&mut cur.docs, &pre.docs, &post.docs);
        self.fields(label, &mut cur.fields, &pre.fields, &post.fields);
        self.plan.tasks.insert(id, cur);
    }

    fn req_added(&mut self, r: &Requirement) {
        let mut nr = r.clone();
        let num: u64 = r.id.trim_start_matches('r').parse().unwrap_or(0);
        if self.plan.reqs.contains_key(&r.id) {
            nr.id = self.plan.new_req_id();
            self.rmap.insert((self.side, r.id.clone()), nr.id.clone());
            self.renumbered.push((self.side, r.id.clone(), nr.id.clone()));
        } else if num > self.plan.next_req {
            self.plan.next_req = num;
        }
        nr.from = self.requirer(&r.from);
        nr.on = self.t(&r.on);
        self.plan.reqs.insert(nr.id.clone(), nr);
    }

    fn req_changed(&mut self, pre: &Requirement, post: &Requirement) {
        let id = self.r(&pre.id);
        let Some(mut cur) = self.plan.reqs.get(&id).cloned() else {
            // Removed by the other side already.
            match self.prefer {
                Some(_) => {
                    if self.wins() {
                        let mut back = post.clone();
                        back.id = id.clone();
                        back.from = self.requirer(&post.from);
                        back.on = self.t(&post.on);
                        self.plan.reqs.insert(id, back);
                    }
                    self.resolved += 1;
                }
                None => self.conflicts.push(format!("{}: changed on one side, removed on the other", id)),
            }
            return;
        };
        let w = |f: &str| format!("{} {}", id, f);
        let (fp, fq) = (self.requirer(&pre.from), self.requirer(&post.from));
        self.field(&w("from"), &mut cur.from, &fp, &fq);
        let (op, oq) = (self.t(&pre.on), self.t(&post.on));
        self.field(&w("on"), &mut cur.on, &op, &oq);
        self.field(&w("reason"), &mut cur.reason, &pre.reason, &post.reason);
        self.field(&w("blocking"), &mut cur.blocking, &pre.blocking, &post.blocking);
        self.field(&w("conditional"), &mut cur.conditional, &pre.conditional, &post.conditional);
        self.fields(&id, &mut cur.fields, &pre.fields, &post.fields);
        self.plan.reqs.insert(id, cur);
    }

    fn req_removed(&mut self, pre: &Requirement) {
        let id = self.r(&pre.id);
        let Some(cur) = self.plan.reqs.get(&id) else { return };
        let mut mapped = pre.clone();
        mapped.id = id.clone();
        mapped.from = self.requirer(&pre.from);
        mapped.on = self.t(&pre.on);
        if *cur == mapped {
            self.plan.reqs.remove(&id);
            return;
        }
        match self.prefer {
            Some(_) => {
                if self.wins() {
                    self.plan.reqs.remove(&id);
                }
                self.resolved += 1;
            }
            None => self.conflicts.push(format!("{}: removed on one side, changed on the other", id)),
        }
    }

    fn type_changed(&mut self, id: &str, pre: Option<&TaskType>, post: Option<&TaskType>) {
        let mut cur = self.plan.types.get(id).cloned();
        self.field(&format!("type {}", id), &mut cur, &pre.cloned(), &post.cloned());
        match cur {
            Some(t) => {
                self.plan.types.insert(id.to_string(), t);
            }
            None => {
                self.plan.types.remove(id);
            }
        }
    }

    /// Apply one event: the difference between its parent's snapshot and
    /// its own.
    fn apply(&mut self, store: &Store, pre: &Snapshot, post: &Snapshot, label: &str) -> Result<()> {
        let pre_plan = Plan::load(store, pre)?;
        let post_plan = Plan::load(store, post)?;
        // Tasks added, in creation order so parents map before children.
        let mut added: Vec<&Task> = post_plan.tasks.values().filter(|t| !pre.tasks.contains_key(&t.id)).collect();
        added.sort_by_key(|t| t.seq);
        for t in added {
            self.task_added(t);
        }
        for (id, h) in &post.tasks {
            if let Some(ph) = pre.tasks.get(id) {
                if ph != h {
                    let name = format!("{} {}", label, post_plan.label(id));
                    self.task_changed(&pre_plan.tasks[id], &post_plan.tasks[id], &name);
                }
            }
        }
        let mut radded: Vec<&Requirement> = post_plan.reqs.values().filter(|r| !pre.reqs.contains_key(&r.id)).collect();
        radded.sort_by_key(|r| r.id.trim_start_matches('r').parse::<u64>().unwrap_or(0));
        for r in radded {
            self.req_added(r);
        }
        for (id, h) in &post.reqs {
            if let Some(ph) = pre.reqs.get(id) {
                if ph != h {
                    self.req_changed(&pre_plan.reqs[id], &post_plan.reqs[id]);
                }
            }
        }
        for id in pre.reqs.keys() {
            if !post.reqs.contains_key(id) {
                self.req_removed(&pre_plan.reqs[id]);
            }
        }
        let tids: BTreeSet<&TypeId> = pre.types.keys().chain(post.types.keys()).collect();
        for id in tids {
            if pre.types.get(id) != post.types.get(id) {
                self.type_changed(id, pre_plan.types.get(id), post_plan.types.get(id));
            }
        }
        Ok(())
    }
}

fn is_status(kind: EventKind) -> bool {
    matches!(kind, EventKind::Started | EventKind::Paused | EventKind::Completed | EventKind::Uncompleted | EventKind::Dropped | EventKind::FlaggedAsSpike)
}

/// Marker events are not replayed: they change no plan and belong to the
/// copy that wrote them.
fn is_marker(kind: EventKind) -> bool {
    matches!(kind, EventKind::Merged | EventKind::Pruned | EventKind::Init)
}

fn identity(h: &Hash, ev: &Event) -> Hash {
    ev.replayed_from.clone().unwrap_or_else(|| h.clone())
}

/// Perform the merge of `theirs` into the store's HEAD. The caller holds
/// the lock. Writes nothing when there are unresolved conflicts or on a
/// dry run.
pub fn perform(store: &Store, theirs: &Hash, prefer: Option<Side>, dry_run: bool) -> Result<Outcome> {
    let mut out = Outcome::default();
    let ours = store.head()?.ok_or_else(|| anyhow!("no HEAD"))?;
    if !store.has_object(theirs) {
        bail!("event {} is not in this store (fetch or pull first: the objects arrive with git)", &theirs[..10]);
    }
    let our_hist = store.history_from(Some(ours.clone()))?;
    let our_hashes: HashSet<&Hash> = our_hist.iter().map(|(h, _)| h).collect();
    // What we already hold: our events and the originals they stand for.
    let have: HashSet<Hash> = our_hist.iter().flat_map(|(h, e)| std::iter::once(h.clone()).chain(e.replayed_from.clone())).collect();
    if have.contains(theirs) {
        out.head = Some(ours);
        return Ok(out);
    }
    // Their events back to the fork point, skipping any we hold already.
    let mut their_seg: Vec<Item> = Vec::new();
    let mut cur = Some(theirs.clone());
    let mut base: Option<Hash> = None;
    let mut skipped = 0usize;
    while let Some(h) = cur {
        if our_hashes.contains(&h) {
            base = Some(h);
            break;
        }
        let ev: Event = store.read_object(ObjectType::Event, &h).with_context(|| format!("event {} of the other history", &h[..10]))?;
        cur = ev.parent.clone();
        let id = identity(&h, &ev);
        if is_marker(ev.kind) || have.contains(&id) || have.contains(&h) {
            skipped += 1;
            continue;
        }
        their_seg.push(Item { id, hash: h, ev, side: Side::Theirs });
    }
    let base = base.ok_or_else(|| anyhow!("{} shares no history with this store", &theirs[..10]))?;
    their_seg.reverse();
    let base_idx = our_hist.iter().position(|(h, _)| *h == base).unwrap();
    // Our events since the fork, oldest first.
    let our_seg: Vec<Item> = our_hist[..base_idx]
        .iter()
        .rev()
        .filter(|(_, e)| !is_marker(e.kind))
        .map(|(h, e)| Item { id: identity(h, e), hash: h.clone(), ev: e.clone(), side: Side::Ours })
        .collect();
    if their_seg.is_empty() {
        out.head = Some(ours);
        return Ok(out);
    }
    if our_seg.is_empty() && skipped == 0 && base == ours {
        // Their line is a pure extension of ours.
        out.fast_forward = true;
        out.head = Some(theirs.clone());
        out.replayed = their_seg.iter().map(|i| (i.hash.clone(), i.hash.clone())).collect();
        if !dry_run {
            store.read_snapshot(&store.read_object::<Event>(ObjectType::Event, theirs)?.snapshot)?;
            store.set_head(theirs)?;
            let plan = Plan::load(store, &store.head_snapshot()?)?;
            let history = store.history()?;
            store.rebuild_log(&history, &plan)?;
        }
        return Ok(out);
    }
    // The canonical order: everything since the fork, chronological, with
    // each side's own order kept (times are to the second, and a side's
    // later event may depend on its earlier one). Ties between the sides
    // go by the original event's hash, the same from either copy.
    let mut all: Vec<Item> = Vec::with_capacity(our_seg.len() + their_seg.len());
    let (mut o, mut t) = (our_seg.into_iter().peekable(), their_seg.into_iter().peekable());
    loop {
        let take_ours = match (o.peek(), t.peek()) {
            (None, None) => break,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (Some(a), Some(b)) => (a.ev.time.as_str(), a.id.as_str()) <= (b.ev.time.as_str(), b.id.as_str()),
        };
        all.push(if take_ours { o.next().unwrap() } else { t.next().unwrap() });
    }
    // Our chain already holds the longest prefix of that order that is ours
    // in order; replay starts after it.
    let mut keep = 0usize;
    for (i, (h, _)) in our_hist[..base_idx].iter().rev().enumerate() {
        match all.get(i) {
            Some(item) if item.side == Side::Ours && item.hash == *h => keep = i + 1,
            _ => break,
        }
    }
    // Markers of ours inside the kept prefix are fine; but a marker between
    // kept and rewritten events would be lost, so count from the chain.
    let start_hash = if keep == 0 { base.clone() } else { all[keep - 1].hash.clone() };
    let start_ev: Event = store.read_object(ObjectType::Event, &start_hash)?;
    let start_snap = store.read_snapshot(&start_ev.snapshot).with_context(|| format!("event {}", &start_hash[..10]))?;
    let plan = Plan::load(store, &start_snap)?;
    let mut rp = Replay { plan, side: Side::Ours, tmap: HashMap::new(), rmap: HashMap::new(), prefer, conflicts: Vec::new(), resolved: 0, renumbered: Vec::new() };
    let mut staged: Vec<(Item, Event, Plan)> = Vec::new();
    for item in all.drain(keep..) {
        rp.side = item.side;
        let parent = item.ev.parent.clone().ok_or_else(|| anyhow!("event {} has no parent", &item.hash[..10]))?;
        let pev: Event = store.read_object(ObjectType::Event, &parent)?;
        let pre = store.read_snapshot(&pev.snapshot).with_context(|| format!("event {}", &parent[..10]))?;
        let post = store.read_snapshot(&item.ev.snapshot).with_context(|| format!("event {}", &item.hash[..10]))?;
        if pre != post {
            rp.apply(store, &pre, &post, &format!("{} {} ({})", item.side.name(), &item.hash[..10], item.ev.kind.name()))?;
        }
        let replayed = Event {
            kind: item.ev.kind,
            parent: None,
            snapshot: String::new(),
            time: item.ev.time.clone(),
            author: item.ev.author.clone(),
            message: item.ev.message.clone(),
            subjects: item.ev.subjects.iter().map(|s| rp.t(s)).collect(),
            group: item.ev.group.iter().map(|s| rp.t(s)).collect(),
            forced: item.ev.forced,
            replayed_from: Some(item.id.clone()),
        };
        if item.side == Side::Ours {
            out.ours_rewritten += 1;
        }
        staged.push((item, replayed, rp.plan.clone()));
    }
    out.conflicts = rp.conflicts.clone();
    out.resolved = rp.resolved;
    out.renumbered = rp.renumbered.clone();
    out.replayed = staged.iter().map(|(i, _, _)| (i.hash.clone(), String::new())).collect();
    // Leaves with status events on both sides: the later one stands. The
    // kept prefix is ours, with final ids already; staged subjects are mapped.
    let mut touched: HashMap<TaskId, (Side, EventKind, String)> = HashMap::new();
    let kept: Vec<(Side, &Event)> = all.iter().map(|i| (i.side, &i.ev)).collect();
    let later: Vec<(Side, &Event)> = staged.iter().map(|(i, e, _)| (i.side, e)).collect();
    for (side, ev) in kept.into_iter().chain(later) {
        if !is_status(ev.kind) {
            continue;
        }
        for s in &ev.subjects {
            if let Some((prev, k, t)) = touched.get(s) {
                if *prev != side {
                    out.both_touched.push(format!("{}: {} {} at {}, then {} {} at {}; the later stands", rp.plan.label(s), prev.name(), k.name(), t, side.name(), ev.kind.name(), ev.time));
                }
            }
            touched.insert(s.clone(), (side, ev.kind, ev.time.clone()));
        }
    }
    if !out.conflicts.is_empty() || dry_run {
        return Ok(out);
    }
    // Write: every replayed event in order, then the merge event.
    let mut prev = start_hash.clone();
    out.replayed.clear();
    for (item, mut ev, plan) in staged {
        ev.parent = Some(prev.clone());
        ev.snapshot = plan.save(store)?;
        let h = store.write_object(ObjectType::Event, &ev)?;
        out.replayed.push((item.hash, h.clone()));
        prev = h;
    }
    store.set_head(&prev)?;
    let mut msg = format!("merge {}: {} event(s) replayed", &theirs[..10], out.replayed.len());
    if out.ours_rewritten > 0 {
        msg.push_str(&format!(", {} of ours reordered", out.ours_rewritten));
    }
    if !out.renumbered.is_empty() {
        let maps: Vec<String> = out.renumbered.iter().map(|(s, a, b)| format!("{} {}->{}", s.name(), a, b)).collect();
        msg.push_str(&format!("; renumbered {}", maps.join(", ")));
    }
    if out.resolved > 0 {
        msg.push_str(&format!("; {} conflict(s) resolved preferring {}", out.resolved, prefer.map(|p| p.name()).unwrap_or("?")));
    }
    let h = commit::write(store, &rp.plan, Commit::new(EventKind::Merged, msg, vec![]))?;
    let history = store.history()?;
    store.rebuild_log(&history, &rp.plan)?;
    out.head = Some(h);
    Ok(out)
}

fn report(store: &Store, theirs: &Hash, out: &Outcome, dry_run: bool) {
    if json_mode() {
        let v = json!({
            "theirs": theirs, "head": out.head, "dry_run": dry_run, "fast_forward": out.fast_forward,
            "replayed": out.replayed.iter().map(|(a, b)| json!({"from": a, "to": b})).collect::<Vec<_>>(),
            "ours_rewritten": out.ours_rewritten,
            "renumbered": out.renumbered.iter().map(|(s, a, b)| json!({"side": s.name(), "from": a, "to": b})).collect::<Vec<_>>(),
            "conflicts": out.conflicts, "resolved": out.resolved, "both_touched": out.both_touched,
        });
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
        return;
    }
    if out.fast_forward {
        say(&format!("fast-forward to {}: {} event(s){}", &theirs[..10], out.replayed.len(), if dry_run { " (dry run)" } else { "" }));
        return;
    }
    if out.replayed.is_empty() && out.conflicts.is_empty() && !dry_run {
        say(&format!("already merged: everything on {} is on this line", &theirs[..10]));
        return;
    }
    for (s, a, b) in &out.renumbered {
        say(&format!("renumbered: {} {} is now {}", s.name(), a, b));
    }
    for b in &out.both_touched {
        say(&format!("status on both sides: {}", b));
    }
    if out.ours_rewritten > 0 {
        say(&format!("{} of our event(s) take a new place in chronological order (new hashes; ids unchanged unless listed)", out.ours_rewritten));
    }
    if !out.conflicts.is_empty() {
        return;
    }
    if dry_run {
        say(&format!("would replay {} event(s){}", out.replayed.len(), if out.resolved > 0 { format!(", resolving {} conflict(s)", out.resolved) } else { String::new() }));
        return;
    }
    if let Some(h) = &out.head {
        done(store, h, &format!("merged {}: {} event(s) replayed", &theirs[..10], out.replayed.len()));
    }
}

/// `tsk merge [THEIRS] [--prefer ours|theirs] [--dry-run] [--git]`.
pub fn merge(theirs: Option<String>, prefer: Option<String>, dry_run: bool, git: bool) -> Result<()> {
    let prefer = Side::parse(prefer)?;
    let store = store_open()?;
    let _lock: Lock = store.lock()?;
    let hf = store.read_head_file()?;
    let theirs = match (theirs, git) {
        (Some(t), _) => store.resolve_hash(&t)?,
        (None, true) => crate::views::git_head_at(&store, "MERGE_HEAD").context("no merge in progress (MERGE_HEAD)")?,
        (None, false) => hf.to_merge().ok_or_else(|| anyhow!("nothing to merge: name the other HEAD (`tsk merge <event>`), or run during a git merge (`--git`)"))?,
    };
    let ours = hf.ours.clone().ok_or_else(|| anyhow!("no HEAD"))?;
    let was_pending = hf.conflicted || hf.pending.is_some();
    if was_pending && !dry_run {
        store.set_head(&ours)?;
    }
    let out = perform(&store, &theirs, prefer, dry_run)?;
    if !out.conflicts.is_empty() {
        if was_pending && !dry_run {
            store.set_head_pending(&ours, &theirs)?;
        }
        report(&store, &theirs, &out, dry_run);
        let mut msg = String::from("conflicts (changed differently on both sides):\n");
        for c in &out.conflicts {
            msg.push_str(&format!("  {}\n", c));
        }
        msg.push_str("nothing was written; run again with --prefer ours or --prefer theirs");
        bail!("{}", msg.trim_end());
    }
    report(&store, &theirs, &out, dry_run);
    Ok(())
}

/// Settle a pending merge named in HEAD before a command runs. For a
/// write, a conflict is an error; for a read, a warning.
pub fn settle_pending(store: &Store, writing: bool) -> Result<()> {
    let hf = store.read_head_file()?;
    let Some(theirs) = hf.to_merge() else { return Ok(()) };
    let ours = hf.ours.clone().ok_or_else(|| anyhow!("HEAD names a merge but no event"))?;
    let lock = match store.lock() {
        Ok(l) => l,
        Err(e) => {
            if writing {
                return Err(e);
            }
            eprintln!("tsk: a merge of {} is pending and the store is locked; showing our side", &theirs[..10]);
            return Ok(());
        }
    };
    // Re-read under the lock: another process may have settled it.
    let hf = store.read_head_file()?;
    let Some(theirs) = hf.to_merge() else { return Ok(()) };
    store.set_head(&ours)?;
    let out = perform(store, &theirs, None, false)?;
    if !out.conflicts.is_empty() {
        store.set_head_pending(&ours, &theirs)?;
        let msg = format!(
            "a merge of {} is pending with {} conflict(s); run `tsk merge --prefer ours|theirs` (or `tsk merge --dry-run` to see them)",
            &theirs[..10],
            out.conflicts.len()
        );
        drop(lock);
        if writing {
            bail!("{}", msg);
        }
        eprintln!("tsk: {}; showing our side", msg);
        return Ok(());
    }
    drop(lock);
    if !json_mode() {
        eprintln!(
            "tsk: merged {} ({} event(s) replayed{})",
            &theirs[..10],
            out.replayed.len(),
            if out.renumbered.is_empty() { String::new() } else { format!(", {} id(s) renumbered", out.renumbered.len()) }
        );
    }
    Ok(())
}

/// The git merge driver for `.tasks/HEAD`: `tsk merge-driver %A %B`. Git
/// calls it before the merged objects reach the working tree, so it does
/// not merge; it records theirs as pending in ours, and the next tsk
/// command performs the merge. Exit 0: git sees no conflict.
pub fn merge_driver(ours_path: String, theirs_path: String) -> Result<()> {
    let first = |p: &str| -> Result<String> {
        let t = std::fs::read_to_string(p).with_context(|| format!("reading {}", p))?;
        Ok(t.lines().next().unwrap_or("").trim().to_string())
    };
    let ours = first(&ours_path)?;
    let theirs = first(&theirs_path)?;
    let text = if ours == theirs || theirs.is_empty() {
        format!("{}\n", ours)
    } else if ours.is_empty() {
        format!("{}\n", theirs)
    } else {
        format!("{}\nmerge {}\n", ours, theirs)
    };
    std::fs::write(&ours_path, text)?;
    Ok(())
}

/// Tell git about the merge driver, globally, and give the store (if one is
/// here) the attributes that use it.
pub fn git_setup() -> Result<()> {
    let run = |args: &[&str]| -> Result<()> {
        let out = std::process::Command::new("git").args(args).output().context("running git")?;
        if !out.status.success() {
            bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(())
    };
    run(&["config", "--global", "merge.tsk.name", "tsk store merge (records the other HEAD; tsk replays it)"])?;
    run(&["config", "--global", "merge.tsk.driver", "tsk merge-driver %A %B"])?;
    say("git: merge.tsk.driver set globally");
    if let Ok(store) = store_open() {
        let p = store.dir.join(".gitattributes");
        let cur = std::fs::read_to_string(&p).unwrap_or_default();
        if !cur.contains("merge=tsk") {
            std::fs::write(&p, crate::store::GITATTRIBUTES)?;
            say(&format!("wrote {}", p.display()));
        }
        let gi = store.dir.join(".gitignore");
        if !gi.exists() {
            std::fs::write(&gi, "# transient files of a running tsk\nlock\nlock.broken-*\nHEAD.tmp\nlog.tmp\n*.EDIT\n*.tmp\n")?;
        }
    }
    Ok(())
}
