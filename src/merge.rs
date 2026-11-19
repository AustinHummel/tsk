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
//!
//! The shared-line merge (`--upstream`, and whatever reaches a keyed store
//! through git) is the other mode: the other HEAD is the line every copy
//! has seen, so it is kept event for event, same hashes, and only this
//! copy's events that are not on it (by hash or by the original they stand
//! for) are replayed after it, in our order. Nothing on their line moves:
//! our new tasks take the next positions, and every number of ours that
//! moved, and every leaf whose status changed on both sides, is reported.

use crate::commands::{done, json_mode, say, store_open};
use crate::commit::{self, Commit};
use crate::model::*;
use crate::plan::{id_number, is_keyed_id, Plan};
use crate::store::{Lock, Store};
use anyhow::{anyhow, bail, Context as _, Result};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

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
    /// The shared-line merge: `moved` and `both` are its report.
    pub upstream: bool,
    /// (our id, outline number before, after) for each task of our
    /// pre-merge plan whose number differs on the merged plan.
    pub moved: Vec<(TaskId, String, String)>,
    pub both: Vec<Both>,
    /// The shared line was our HEAD, not theirs (a stash pop or a
    /// cherry-pick): its ref. It was kept and their events replayed after it.
    pub swapped: Option<String>,
}

/// A leaf whose status changed on both sides of a shared-line merge.
pub struct Both {
    pub id: TaskId,
    /// The status it ends in on the merged line.
    pub ends: &'static str,
    /// Whether our status event, which stands because it is replayed after
    /// theirs, is the older of the two.
    pub older_stands: bool,
    pub text: String,
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
    /// A keyed id made on both sides: the merge is refused.
    clash: Option<String>,
    /// The shared-line merge: a task of ours that keeps its id still takes
    /// the next position from the merged counter.
    upstream: bool,
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
        let num = id_number(&t.id);
        if is_keyed_id(&t.id) {
            // A keyed id is never renamed. Made on both sides, it is the
            // same key on two copies: refused. Otherwise it keeps its id and,
            // when the counter has passed its number (the other copy minted
            // that number under its own key), takes the next position, so
            // no two siblings share a sequence number.
            if self.plan.tasks.contains_key(&t.id) {
                self.clash.get_or_insert_with(|| t.id.clone());
                return;
            }
            if num > self.plan.next_task {
                self.plan.next_task = num;
                if self.upstream {
                    nt.seq = num;
                }
            } else {
                self.plan.next_task += 1;
                nt.seq = self.plan.next_task;
            }
        // Renumber when the id is taken, or when the merged line's counter
        // has already passed it (an `adopt` draws a sequence number from
        // the counter without creating a task), so no two tasks share a
        // sequence number. The new id is bare on any store: every copy
        // must compute the same one.
        } else if self.plan.tasks.contains_key(&t.id) || num <= self.plan.next_task {
            nt.id = self.plan.new_bare_task_id();
            nt.seq = self.plan.next_task;
            self.tmap.insert((self.side, t.id.clone()), nt.id.clone());
            self.renumbered.push((self.side, t.id.clone(), nt.id.clone()));
        } else if num > self.plan.next_task {
            self.plan.next_task = num;
            if self.upstream {
                nt.seq = num;
            }
        }
        nt.parent = t.parent.as_deref().map(|p| self.t(p));
        nt.core_of = t.core_of.as_deref().map(|p| self.t(p));
        self.plan.tasks.insert(nt.id.clone(), nt);
        self.plan.invalidate();
    }

    fn task_changed(&mut self, pre: &Task, post: &Task, label: &str) {
        let id = self.t(&pre.id);
        let Some(mut cur) = self.plan.tasks.get(&id).cloned() else {
            self.conflicts.push(format!("{}: changed but missing on the merged line", label));
            return;
        };
        let w = |f: &str| format!("{} {}", label, f);
        self.field(&w("title"), &mut cur.title, &pre.title, &post.title);
        let held = Former { parent: cur.parent.clone(), seq: cur.seq };
        let (pp, pq) = (pre.parent.as_deref().map(|p| self.t(p)), post.parent.as_deref().map(|p| self.t(p)));
        self.field(&w("parent"), &mut cur.parent, &pp, &pq);
        if post.seq != pre.seq && cur.parent == pq {
            // An adoption: on its own line the task took the next position
            // from the counter. On this line it takes the next position
            // from this counter, and the slot it leaves vacant is the one
            // it held here (its id and position may both have been
            // renumbered), so positions stay unique and no sibling moves.
            cur.former.push(held);
            self.plan.next_task += 1;
            cur.seq = self.plan.next_task;
        } else {
            let map_former = |v: &[Former]| -> Vec<Former> { v.iter().map(|f| Former { parent: f.parent.as_deref().map(|p| self.t(p)), seq: f.seq }).collect() };
            let (fp, fq) = (map_former(&pre.former), map_former(&post.former));
            self.field(&w("former positions"), &mut cur.former, &fp, &fq);
        }
        self.field(&w("body"), &mut cur.body, &pre.body, &post.body);
        self.field(&w("narrative"), &mut cur.narrative, &pre.narrative, &post.narrative);
        self.field(&w("fit"), &mut cur.fit, &pre.fit, &post.fit);
        self.field(&w("ongoing"), &mut cur.ongoing, &pre.ongoing, &post.ongoing);
        self.field(&w("after"), &mut cur.after, &pre.after, &post.after);
        self.field(&w("priority"), &mut cur.priority, &pre.priority, &post.priority);
        self.field(&w("due"), &mut cur.due, &pre.due, &post.due);
        self.field(&w("from_item"), &mut cur.from_item, &pre.from_item, &post.from_item);
        self.list(&mut cur.types, &pre.types, &post.types);
        self.list(&mut cur.docs, &pre.docs, &post.docs);
        self.fields(label, &mut cur.fields, &pre.fields, &post.fields);
        self.plan.tasks.insert(id, cur);
        self.plan.invalidate();
    }

    fn req_added(&mut self, r: &Requirement) {
        let mut nr = r.clone();
        let num = id_number(&r.id);
        if is_keyed_id(&r.id) && self.plan.reqs.contains_key(&r.id) {
            self.clash.get_or_insert_with(|| r.id.clone());
            return;
        }
        if self.plan.reqs.contains_key(&r.id) {
            nr.id = self.plan.new_bare_req_id();
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
        radded.sort_by_key(|r| (id_number(&r.id), r.id.clone()));
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
    matches!(kind, EventKind::Started | EventKind::Paused | EventKind::Completed | EventKind::Uncompleted | EventKind::Dropped | EventKind::FlaggedAsSpike | EventKind::Closed | EventKind::Reopened)
}

/// An event that adds scope under its first subject.
fn gains_scope(kind: EventKind) -> bool {
    matches!(kind, EventKind::Amended | EventKind::Adopted | EventKind::BrokenDown)
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
/// `swap` names the shared line's ref when it is our HEAD, not theirs
/// (`orient`): the shared-line merge then keeps ours and replays theirs.
pub fn perform(store: &Store, theirs: &Hash, prefer: Option<Side>, dry_run: bool, upstream: bool, swap: Option<String>) -> Result<Outcome> {
    if upstream {
        perform_upstream(store, theirs, prefer, dry_run, swap)
    } else {
        perform_canonical(store, theirs, prefer, dry_run)
    }
}

fn perform_canonical(store: &Store, theirs: &Hash, prefer: Option<Side>, dry_run: bool) -> Result<Outcome> {
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
            store.advance_head(Some(&ours), theirs, &format!("merge {} (fast-forward)", &theirs[..10]))?;
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
    let mut rp = Replay::new(plan, prefer, false);
    let staged = stage(store, &mut rp, all.drain(keep..).collect(), &mut out)?;
    refuse_clash(&rp, &base)?;
    out.conflicts = rp.conflicts.clone();
    out.resolved = rp.resolved;
    out.renumbered = rp.renumbered.clone();
    out.replayed = staged.iter().map(|(i, _, _)| (i.hash.clone(), String::new())).collect();
    // Leaves with status events on both sides: the later one stands. The
    // kept prefix is ours, with final ids already; staged subjects are mapped.
    let mut touched: HashMap<TaskId, (Side, EventKind, String)> = HashMap::new();
    let kept: Vec<(Side, &Event)> = all.iter().map(|i| (i.side, &i.ev)).collect();
    let later: Vec<(Side, &Event)> = staged.iter().map(|(i, e, _)| (i.side, e)).collect();
    // A close on one side and scope gained under the same task on the
    // other: the merged derivation reopens it on its own; the person is
    // told.
    let mut closed_on: Vec<(Side, TaskId, String)> = Vec::new();
    let mut gained_on: Vec<(Side, TaskId, Vec<TaskId>)> = Vec::new();
    for (side, ev) in kept.into_iter().chain(later) {
        if ev.kind == EventKind::Closed {
            for s in &ev.subjects {
                closed_on.push((side, s.clone(), ev.time.clone()));
            }
        }
        if gains_scope(ev.kind) {
            if let Some(p) = ev.subjects.first() {
                gained_on.push((side, p.clone(), ev.subjects[1..].to_vec()));
            }
        }
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
    for (cs, task, time) in &closed_on {
        for (gs, p, kids) in &gained_on {
            if cs != gs && p == task {
                let names: Vec<String> = kids.iter().map(|k| rp.plan.label(k)).collect();
                out.both_touched.push(format!("{}: closed on {} at {}, then gained {} on {}; it is open again", rp.plan.label(task), cs.name(), time, names.join(", "), gs.name()));
            }
        }
    }
    if !out.conflicts.is_empty() || dry_run {
        return Ok(out);
    }
    // Write: every replayed event in order, then the merge event. The
    // claim on our HEAD comes first, so nothing is written if another
    // process has advanced it meanwhile.
    store.claim(&ours)?;
    let prev = write_staged(store, staged, &start_hash, &mut out)?;
    store.set_head(&prev)?;
    let mut msg = format!("merge {} (from {}): {} event(s) replayed", &theirs[..10], &ours[..10], out.replayed.len());
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
    let h = commit::write_on(store, &rp.plan, Some(prev.clone()), Commit::new(EventKind::Merged, msg, vec![]))?;
    // One reflog line for the whole merge: the way back is `tsk reset <old>`.
    store.reflog_append(Some(&ours), &h, &format!("merge {}: HEAD was {}", &theirs[..10], &ours[..10]))?;
    let history = store.history()?;
    store.rebuild_log(&history, &rp.plan)?;
    out.head = Some(h);
    Ok(out)
}

impl Replay {
    fn new(plan: Plan, prefer: Option<Side>, upstream: bool) -> Replay {
        Replay { plan, side: Side::Ours, tmap: HashMap::new(), rmap: HashMap::new(), prefer, conflicts: Vec::new(), resolved: 0, renumbered: Vec::new(), clash: None, upstream }
    }
}

/// Replay each item onto the plan in `rp`, in the order given: the merged
/// plan after each one, and the event that will stand for it (no parent or
/// snapshot yet; ids mapped; `replayed_from` naming the original).
fn stage(store: &Store, rp: &mut Replay, items: Vec<Item>, out: &mut Outcome) -> Result<Vec<(Item, Event, Plan)>> {
    let mut staged: Vec<(Item, Event, Plan)> = Vec::new();
    for item in items {
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
    Ok(staged)
}

/// A keyed id made on both copies refuses the merge, nothing written.
fn refuse_clash(rp: &Replay, base: &Hash) -> Result<()> {
    if let Some(id) = &rp.clash {
        let key: String = id.chars().skip_while(|c| !c.is_ascii_digit()).skip_while(|c| c.is_ascii_digit()).collect();
        bail!(
            "{} was made on both copies, which share the key `{}`, and a keyed id is never renamed, so nothing was written: on one copy choose a key no other copy uses (`tsk key <letters>`), `tsk reset {}` there to undo its work since the fork, add that work again, and merge",
            id,
            key,
            &base[..10]
        );
    }
    Ok(())
}

/// Write the staged events in order on top of `start`; the last one's hash.
fn write_staged(store: &Store, staged: Vec<(Item, Event, Plan)>, start: &Hash, out: &mut Outcome) -> Result<Hash> {
    let mut prev = start.clone();
    out.replayed.clear();
    for (item, mut ev, plan) in staged {
        ev.parent = Some(prev.clone());
        ev.snapshot = plan.save(store)?;
        let h = store.write_object(ObjectType::Event, &ev)?;
        out.replayed.push((item.hash, h.clone()));
        prev = h;
    }
    Ok(prev)
}

/// Each task of our plan before the merge whose outline number differs on
/// the merged plan (followed through a rename of ours).
fn moved(before: &Plan, after: &Plan, tmap: &HashMap<(Side, TaskId), TaskId>) -> Vec<(TaskId, String, String)> {
    let (b, a) = (before.numbering(), after.numbering());
    let mut v: Vec<(TaskId, String, String)> = b
        .iter()
        .filter_map(|(id, from)| {
            let now = tmap.get(&(Side::Ours, id.clone())).unwrap_or(id);
            let to = a.get(now)?;
            (to != from).then(|| (id.clone(), from.clone(), to.clone()))
        })
        .collect();
    v.sort_by_key(|(id, _, _)| (id_number(id), id.clone()));
    v
}

/// The shared-line merge. `theirs` is the line every copy has seen: it is
/// kept event for event, same hashes. Our events since the fork that are
/// not on it, by hash or by the original they stand for, are replayed after
/// its head in our order, then one `Merged` event. Nothing of ours left is
/// a fast-forward to their head.
fn perform_upstream(store: &Store, theirs: &Hash, prefer: Option<Side>, dry_run: bool, swap: Option<String>) -> Result<Outcome> {
    let mut out = Outcome { upstream: true, swapped: swap.clone(), ..Outcome::default() };
    let head = store.head()?.ok_or_else(|| anyhow!("no HEAD"))?;
    if !store.has_object(theirs) {
        bail!("event {} is not in this store (fetch or pull first: the objects arrive with git)", &theirs[..10]);
    }
    // Below, `ours` is the copy whose events are replayed and `theirs` the
    // shared line kept; swapped, the shared line is our HEAD. HEAD moves
    // from `head` either way.
    let (ours, theirs) = if swap.is_some() { (theirs.clone(), head.clone()) } else { (head.clone(), theirs.clone()) };
    let theirs = &theirs;
    let our_hist = store.history_from(Some(ours.clone()))?;
    let our_hashes: HashSet<&Hash> = our_hist.iter().map(|(h, _)| h).collect();
    // Their line is the authority: their head on ours by hash, and only by
    // hash, is already merged.
    if our_hashes.contains(theirs) {
        out.head = Some(ours);
        return Ok(out);
    }
    let their_hist = store.history_from(Some(theirs.clone())).context("reading the other history")?;
    let their_hashes: HashSet<&Hash> = their_hist.iter().map(|(h, _)| h).collect();
    // Everything their line holds: its events and the originals they stand for.
    let on_theirs: HashSet<&Hash> = their_hist.iter().flat_map(|(h, e)| std::iter::once(h).chain(e.replayed_from.iter())).collect();
    let base_idx = our_hist.iter().position(|(h, _)| their_hashes.contains(h)).ok_or_else(|| anyhow!("{} shares no history with this store", &theirs[..10]))?;
    let base = our_hist[base_idx].0.clone();
    let our_seg: Vec<Item> = our_hist[..base_idx]
        .iter()
        .rev()
        .filter(|(h, e)| !is_marker(e.kind) && !on_theirs.contains(h) && !on_theirs.contains(&identity(h, e)))
        .map(|(h, e)| Item { id: identity(h, e), hash: h.clone(), ev: e.clone(), side: Side::Ours })
        .collect();
    let before = store.load_plan(&our_hist[0].1.snapshot)?;
    let their_snap = &their_hist[0].1.snapshot;
    // Their events since the fork, oldest first, less any that stand for an
    // event of ours.
    let have: HashSet<Hash> = our_hist.iter().flat_map(|(h, e)| std::iter::once(h.clone()).chain(e.replayed_from.clone())).collect();
    let their_new: Vec<&Event> = their_hist.iter().take_while(|(h, _)| !our_hashes.contains(h)).filter(|(h, e)| !have.contains(&identity(h, e))).map(|(_, e)| e).collect::<Vec<_>>().into_iter().rev().collect();
    if our_seg.is_empty() && *theirs == head {
        // Swapped, with nothing of theirs off our line: already merged.
        out.head = Some(head);
        return Ok(out);
    }
    if our_seg.is_empty() {
        out.fast_forward = true;
        out.head = Some(theirs.clone());
        out.replayed = their_hist.iter().take_while(|(h, _)| !our_hashes.contains(h)).map(|(h, _)| (h.clone(), h.clone())).collect();
        let merged = store.load_plan(their_snap)?;
        out.moved = moved(&before, &merged, &HashMap::new());
        if !dry_run {
            store.advance_head(Some(&head), theirs, &format!("merge {} (fast-forward)", &theirs[..10]))?;
            let history = store.history()?;
            store.rebuild_log(&history, &merged)?;
        }
        return Ok(out);
    }
    // Replay reads each event's own snapshot and its parent's: a prune past
    // our unpublished work refuses here, before anything is written.
    for item in &our_seg {
        let parent = item.ev.parent.clone().ok_or_else(|| anyhow!("event {} has no parent", &item.hash[..10]))?;
        let pev: Event = store.read_object(ObjectType::Event, &parent)?;
        if !store.has_object(&pev.snapshot) || !store.has_object(&item.ev.snapshot) {
            bail!(
                "event {} of ours, which this merge replays, lost a snapshot to a prune, so nothing was written: restore `.tasks/objects` from a git commit made before the prune (`git checkout <commit> -- .tasks/objects`), then merge again",
                &item.hash[..10]
            );
        }
    }
    let mut rp = Replay::new(store.load_plan(their_snap)?, prefer, true);
    let staged = stage(store, &mut rp, our_seg, &mut out)?;
    refuse_clash(&rp, &base)?;
    out.conflicts = rp.conflicts.clone();
    out.resolved = rp.resolved;
    out.renumbered = rp.renumbered.clone();
    out.replayed = staged.iter().map(|(i, _, _)| (i.hash.clone(), String::new())).collect();
    out.moved = moved(&before, &rp.plan, &rp.tmap);
    // Status on both sides: ours is replayed after theirs, so ours stands,
    // whichever is older. Say what it ends in.
    let last = |evs: &mut dyn Iterator<Item = &Event>| -> BTreeMap<TaskId, (EventKind, String)> {
        let mut m = BTreeMap::new();
        for e in evs.filter(|e| is_status(e.kind)) {
            for s in &e.subjects {
                m.insert(s.clone(), (e.kind, e.time.clone()));
            }
        }
        m
    };
    let their_last = last(&mut their_new.iter().copied());
    let our_last = last(&mut staged.iter().map(|(_, e, _)| e));
    let mut ends = crate::state::leaf_statuses(&their_hist);
    for (_, e, _) in &staged {
        crate::state::apply_event(&mut ends, e);
    }
    for (id, (ok, ot)) in &our_last {
        if let Some((tk, tt)) = their_last.get(id) {
            let end = ends.get(id).map(|s| s.name()).unwrap_or("todo");
            let older = ot < tt;
            out.both.push(Both {
                id: id.clone(),
                ends: end,
                older_stands: older,
                text: format!("{} ends {} (ours {} {}, theirs {} {}; the {} event stands)", id, end, ok.name(), ot, tk.name(), tt, if older { "older" } else { "later" }),
            });
        }
    }
    if !out.conflicts.is_empty() || dry_run {
        return Ok(out);
    }
    store.claim(&head)?;
    let prev = write_staged(store, staged, theirs, &mut out)?;
    store.set_head(&prev)?;
    let mut msg = match &swap {
        None => format!("merge {} (from {}): shared line kept, {} event(s) of ours replayed after it", &theirs[..10], &ours[..10], out.replayed.len()),
        Some(r) => format!("merge {} (from {}): sides swapped, the shared line ({}) was ours: kept, {} event(s) of theirs replayed after it", &ours[..10], &head[..10], r, out.replayed.len()),
    };
    if !out.renumbered.is_empty() {
        let maps: Vec<String> = out.renumbered.iter().map(|(s, a, b)| format!("{} {}->{}", s.name(), a, b)).collect();
        msg.push_str(&format!("; renumbered {}", maps.join(", ")));
    }
    if !out.moved.is_empty() {
        let m: Vec<String> = out.moved.iter().map(|(id, a, b)| format!("{} {}->{}", id, a, b)).collect();
        msg.push_str(&format!("; moved {}", m.join(", ")));
    }
    if !out.both.is_empty() {
        let b: Vec<&str> = out.both.iter().map(|b| b.text.as_str()).collect();
        msg.push_str(&format!("; status on both sides: {}", b.join("; ")));
    }
    if out.resolved > 0 {
        msg.push_str(&format!("; {} conflict(s) resolved preferring {}", out.resolved, prefer.map(|p| p.name()).unwrap_or("?")));
    }
    let h = commit::write_on(store, &rp.plan, Some(prev.clone()), Commit::new(EventKind::Merged, msg, vec![]))?;
    store.reflog_append(Some(&head), &h, &format!("merge {} (shared line): HEAD was {}", &theirs[..10], &head[..10]))?;
    let history = store.history()?;
    store.rebuild_log(&history, &rp.plan)?;
    out.head = Some(h);
    Ok(out)
}

fn report(store: &Store, theirs: &Hash, out: &Outcome, dry_run: bool) {
    if json_mode() {
        let mut v = json!({
            "theirs": theirs, "head": out.head, "dry_run": dry_run, "fast_forward": out.fast_forward,
            "replayed": out.replayed.iter().map(|(a, b)| json!({"from": a, "to": b})).collect::<Vec<_>>(),
            "ours_rewritten": out.ours_rewritten,
            "renumbered": out.renumbered.iter().map(|(s, a, b)| json!({"side": s.name(), "from": a, "to": b})).collect::<Vec<_>>(),
            "conflicts": out.conflicts, "resolved": out.resolved,
            "both_touched": if out.upstream {
                out.both.iter().map(|b| json!({"id": b.id, "ends": b.ends, "older_stands": b.older_stands, "text": b.text})).collect::<Vec<_>>()
            } else {
                out.both_touched.iter().map(|b| json!(b)).collect::<Vec<_>>()
            },
        });
        if out.upstream {
            v["upstream"] = json!(true);
            v["swapped"] = json!(out.swapped.is_some());
            v["moved"] = json!(out.moved.iter().map(|(id, a, b)| json!({"id": id, "from": a, "to": b})).collect::<Vec<_>>());
        }
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
        return;
    }
    let say_moved = || {
        for (id, a, b) in &out.moved {
            say(&format!("moved: {} {} -> {}", id, a, b));
        }
    };
    if let Some(r) = &out.swapped {
        say(&swapped_note(r));
    }
    if out.fast_forward {
        say_moved();
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
    say_moved();
    for b in out.both_touched.iter().chain(out.both.iter().map(|b| &b.text)) {
        say(&format!("status on both sides: {}", b));
    }
    if out.upstream && out.ours_rewritten > 0 {
        say(&format!("{} of our event(s) replayed after the shared line (new hashes; ids unchanged unless listed)", out.ours_rewritten));
    } else if out.ours_rewritten > 0 {
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

/// `tsk merge [THEIRS] [--prefer ours|theirs] [--dry-run] [--git] [--upstream]`.
pub fn merge(theirs: Option<String>, prefer: Option<String>, dry_run: bool, git: bool, upstream: bool) -> Result<()> {
    let prefer = Side::parse(prefer)?;
    let store = store_open()?;
    let _lock: Lock = store.lock()?;
    let hf = store.read_head_file()?;
    // On a keyed store, what came through git (the driver's `merge` line,
    // conflict markers, or MERGE_HEAD) is the shared line.
    let inferred = !upstream && store.keyed() && (theirs.is_none() || git);
    let upstream = upstream || inferred;
    let theirs = match (theirs, git) {
        (Some(t), _) => store.resolve_hash(&t)?,
        (None, true) => crate::views::git_head_at(&store, "MERGE_HEAD").context("no merge in progress (MERGE_HEAD)")?,
        (None, false) => hf.to_merge().ok_or_else(|| anyhow!("nothing to merge: name the other HEAD (`tsk merge <event>`), or run during a git merge (`--git`)"))?,
    };
    let ours = hf.ours.clone().ok_or_else(|| anyhow!("no HEAD"))?;
    let was_pending = hf.conflicted || hf.pending.is_some();
    let swap = if inferred { orient(&store, &ours, &theirs)? } else { None };
    let out = perform(&store, &theirs, prefer, dry_run, upstream, swap)?;
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
    if was_pending && !dry_run && out.head.as_ref() == Some(&ours) && hf.to_merge().as_ref() == Some(&theirs) {
        // Already merged: clear the pending line, or every command re-runs it.
        store.set_head(&ours)?;
    }
    report(&store, &theirs, &out, dry_run);
    Ok(())
}

/// The shared line's git ref: config `shared`, else `origin/main`.
fn shared_ref(store: &Store) -> String {
    store.config_value("shared").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| "origin/main".to_string())
}

fn swapped_note(r: &str) -> String {
    format!("sides swapped: the shared line ({}) was our HEAD, not theirs, as after a stash pop or a cherry-pick, so it was kept and theirs replayed after it", r)
}

/// Which side of a merge that came through git holds the shared line: the
/// store HEAD committed at the shared ref. git's sides are ours and theirs
/// in a merge or a pull, but the other way round in a stash pop or a
/// cherry-pick, and the driver cannot tell them apart. Theirs holds it:
/// settle as recorded (None). Only ours holds it: settle with the sides
/// swapped (the ref's name). Neither, or the ref can't be read: refused,
/// nothing written.
fn orient(store: &Store, ours: &Hash, theirs: &Hash) -> Result<Option<String>> {
    let holds = |line: &Hash, h: &Hash| -> Result<bool> { Ok(store.history_from(Some(line.clone()))?.iter().any(|(x, _)| x == h)) };
    // Missing, or already on our line: the merge itself says so.
    if !store.has_object(theirs) || holds(ours, theirs)? {
        return Ok(None);
    }
    let r = shared_ref(store);
    let way = format!("name the shared line with `tsk config shared <ref>`, or, if {} is the shared line, run `tsk merge --upstream`", &theirs[..10]);
    let s = crate::views::git_head_at(store, &r)
        .map_err(|e| anyhow!("tsk cannot read the shared line's store head at {} ({}), so it cannot tell which side of this merge to keep, and nothing was written: run `git fetch`, {}", r, e, way))?;
    if holds(theirs, &s)? {
        return Ok(None);
    }
    if holds(ours, &s)? {
        return Ok(Some(r));
    }
    bail!(
        "neither side of this merge holds the shared line's store head ({} at {}), so tsk cannot tell which side to keep, and nothing was written: merge {} first (`git merge {}`), {}",
        &s[..10],
        r,
        r,
        r,
        way
    )
}

/// Settle a pending merge named in HEAD before a command runs. For a
/// write, a conflict is an error; for a read, a warning.
pub fn settle_pending(store: &Store, writing: bool) -> Result<()> {
    let hf = store.read_head_file()?;
    let Some(theirs) = hf.to_merge() else { return Ok(()) };
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
    let ours = hf.ours.clone().ok_or_else(|| anyhow!("HEAD names a merge but no event"))?;
    // A keyed store settles what git brought as the shared-line merge, on
    // whichever side holds the shared line. A read that cannot settle (the
    // shared line can't be told, or a live writer holds the claim) shows
    // our side; only a write is refused.
    let settled = (|| {
        let swap = if store.keyed() { orient(store, &ours, &theirs)? } else { None };
        perform(store, &theirs, None, false, store.keyed(), swap)
    })();
    let out = match settled {
        Ok(out) => out,
        Err(e) if !writing => {
            drop(lock);
            eprintln!("tsk: a merge of {} is pending and was not settled: {}; showing our side", &theirs[..10], e);
            return Ok(());
        }
        Err(e) => return Err(e),
    };
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
    if out.head.as_ref() == Some(&ours) {
        // Already merged: the pending line goes, HEAD stays.
        store.set_head(&ours)?;
        drop(lock);
        if !json_mode() {
            eprintln!("tsk: {} is already merged; the pending merge is cleared", &theirs[..10]);
        }
        return Ok(());
    }
    drop(lock);
    if !json_mode() {
        eprintln!(
            "tsk: merged {} ({}{} event(s) replayed{})",
            &theirs[..10],
            if out.upstream { "shared line kept, " } else { "" },
            out.replayed.len(),
            if out.renumbered.is_empty() { String::new() } else { format!(", {} id(s) renumbered", out.renumbered.len()) }
        );
        if let Some(r) = &out.swapped {
            eprintln!("tsk: {}", swapped_note(r));
        }
        for (id, a, b) in &out.moved {
            eprintln!("tsk: moved: {} {} -> {}", id, a, b);
        }
        for b in &out.both {
            eprintln!("tsk: status on both sides: {}", b.text);
        }
    }
    Ok(())
}

/// The git merge driver for `.tasks/HEAD`: `tsk merge-driver %A %B %P`.
/// Git calls it before the merged objects reach the working tree, so it
/// does not merge; it records theirs as pending in ours, and the next tsk
/// command performs the merge. Exit 0: git sees no conflict.
///
/// On a keyed store it refuses during a rebase, which swaps the sides (%A
/// is the shared line), so the next command would replay the shared line
/// instead of keeping it: %A untouched, exit 1, and git stops on a
/// conflict. git runs the driver from the working tree's top and %P is the
/// path from there; with no %P (a 0.6.0 registration) the store is
/// `./.tasks`.
pub fn merge_driver(ours_path: String, theirs_path: String, path: Option<String>) -> Result<()> {
    let dir = match path.as_deref().map(std::path::Path::new).and_then(|p| p.parent()) {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        Some(_) => std::path::PathBuf::from("."),
        None => std::path::PathBuf::from(".tasks"),
    };
    if Store::at(&dir).is_ok_and(|s| s.keyed()) && rebasing() {
        bail!("this store is keyed and git is rebasing, which swaps the sides, so the shared line would be replayed instead of kept; run `git rebase --abort`, then `git merge <the shared line>`");
    }
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

/// Whether git is in the middle of a rebase: either backend's state
/// directory exists. Asked of git, since a linked worktree keeps it under
/// `.git/worktrees/<name>/`.
fn rebasing() -> bool {
    ["rebase-merge", "rebase-apply"].iter().any(|d| {
        std::process::Command::new("git")
            .args(["rev-parse", "--git-path", d])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| std::path::Path::new(String::from_utf8_lossy(&o.stdout).trim()).exists())
            .unwrap_or(false)
    })
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
    run(&["config", "--global", "merge.tsk.driver", "tsk merge-driver %A %B %P"])?;
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
