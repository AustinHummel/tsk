//! Everything derived: status from the event log, the expanded dependency
//! graph, loops, and readiness. Nothing here is ever stored.

use crate::model::*;
use crate::plan::Plan;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum Status {
    Todo,
    Spike,
    InProgress,
    Paused,
    Complete,
    Dropped,
}

impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Status::Todo => "todo",
            Status::Spike => "spike",
            Status::InProgress => "in-progress",
            Status::Paused => "paused",
            Status::Complete => "complete",
            Status::Dropped => "dropped",
        }
    }
    pub fn glyph(self) -> &'static str {
        match self {
            Status::Todo => "[ ]",
            Status::Spike => "[?]",
            Status::InProgress => "[>]",
            Status::Paused => "[=]",
            Status::Complete => "[x]",
            Status::Dropped => "[-]",
        }
    }
}

/// Fold one event into the per-leaf status map.
pub fn apply_event(out: &mut HashMap<TaskId, Status>, ev: &Event) {
    let st = match ev.kind {
        EventKind::Started => Status::InProgress,
        EventKind::Paused => Status::Paused,
        EventKind::Completed => Status::Complete,
        EventKind::Uncompleted => Status::Todo,
        EventKind::Dropped => Status::Dropped,
        EventKind::FlaggedAsSpike => Status::Spike,
        EventKind::BrokenDown | EventKind::Applied | EventKind::Adopted | EventKind::Amended => Status::Todo,
        _ => return,
    };
    if matches!(ev.kind, EventKind::Adopted | EventKind::Amended) {
        // The parent (first subject) may have been a leaf until now: its
        // own status ends. The moved or added tasks keep theirs.
        if let Some(p) = ev.subjects.first() {
            out.remove(p);
        }
        return;
    }
    for s in &ev.subjects {
        if ev.kind == EventKind::BrokenDown || ev.kind == EventKind::Applied {
            // The parent's own status ends; the children carry it on.
            out.remove(s);
        } else {
            out.insert(s.clone(), st);
        }
    }
}

/// The last status-changing event per leaf, read from the history.
pub fn leaf_statuses(history: &[(Hash, Event)]) -> HashMap<TaskId, Status> {
    let mut out: HashMap<TaskId, Status> = HashMap::new();
    // history is newest first; walk oldest first so later events win.
    for (_, ev) in history.iter().rev() {
        apply_event(&mut out, ev);
    }
    out
}

pub struct Derived {
    pub status: BTreeMap<TaskId, Status>,
    /// Expanded edges: predecessor leaf -> successor leaf, with the
    /// requirement ids that put them there.
    /// The expanded edges keyed by successor, for `blocks`.
    pub in_edges: HashMap<TaskId, Vec<(TaskId, BTreeSet<ReqId>)>>,
    /// Requirement ids placed by each task.
    pub reqs_from: HashMap<TaskId, Vec<ReqId>>,
    /// Strongly connected components of size > 1 or with a self edge.
    pub loops: Vec<Loop>,
    pub in_loop: HashMap<TaskId, usize>,
}

#[derive(Debug, Clone)]
pub struct Loop {
    pub tasks: Vec<TaskId>,
    pub reqs: BTreeSet<ReqId>,
}

#[derive(Debug, Clone)]
pub enum Block {
    Loop(usize),
    /// A conditional requirement; the bool says whether its condition task
    /// is complete (then the task must be confirmed or dropped).
    Tentative(ReqId, bool),
    /// A requirement whose named predecessor still has open work under it:
    /// the predecessor as the requirement names it, the requirement, and
    /// how many open tasks under the predecessor remain.
    Waiting(TaskId, ReqId, usize),
}

impl Derived {
    pub fn compute(plan: &Plan, leaf: &HashMap<TaskId, Status>) -> Derived {
        let leaf = leaf.clone();
        let mut status = BTreeMap::new();
        let mut memo: HashMap<TaskId, Status> = HashMap::new();
        for id in plan.tasks.keys() {
            let st = status_of(plan, &leaf, id, &mut memo);
            status.insert(id.clone(), st);
        }
        let mut reqs_from: HashMap<TaskId, Vec<ReqId>> = HashMap::new();
        for r in plan.reqs.values() {
            if let Requirer::Task(t) = &r.from {
                reqs_from.entry(t.clone()).or_default().push(r.id.clone());
            }
        }

        // Expand each blocking requirement from=F on=P into edges from every
        // task under P to every task under F. Dropped tasks leave the graph.
        let live = |id: &TaskId| status.get(id).map(|s| *s != Status::Dropped).unwrap_or(false);
        let mut edges: BTreeMap<(TaskId, TaskId), BTreeSet<ReqId>> = BTreeMap::new();
        for r in plan.reqs.values() {
            if !r.blocking {
                continue;
            }
            let from = match &r.from {
                Requirer::Task(t) => t,
                Requirer::Person(_) => continue,
            };
            if !plan.tasks.contains_key(from) || !plan.tasks.contains_key(&r.on) {
                continue;
            }
            for p in plan.subtree(&r.on) {
                if !live(&p) {
                    continue;
                }
                for f in plan.subtree(from) {
                    if !live(&f) {
                        continue;
                    }
                    edges.entry((p.clone(), f.clone())).or_default().insert(r.id.clone());
                }
            }
        }

        // Loops among tasks that are not yet complete.
        let open: Vec<TaskId> = plan
            .tasks
            .keys()
            .filter(|id| matches!(status.get(*id), Some(Status::Todo | Status::Spike | Status::InProgress | Status::Paused)))
            .cloned()
            .collect();
        let open_set: HashSet<&TaskId> = open.iter().collect();
        let mut adj: HashMap<&TaskId, Vec<&TaskId>> = HashMap::new();
        for (p, f) in edges.keys() {
            if open_set.contains(p) && open_set.contains(f) {
                adj.entry(p).or_default().push(f);
            }
        }
        let sccs = tarjan(&open, &adj);
        let mut loops = Vec::new();
        let mut in_loop = HashMap::new();
        for scc in sccs {
            let is_loop = scc.len() > 1 || edges.contains_key(&(scc[0].clone(), scc[0].clone()));
            if !is_loop {
                continue;
            }
            let set: HashSet<&TaskId> = scc.iter().collect();
            let mut reqs = BTreeSet::new();
            for ((p, f), rs) in &edges {
                if set.contains(p) && set.contains(f) {
                    reqs.extend(rs.iter().cloned());
                }
            }
            let idx = loops.len();
            for t in &scc {
                in_loop.insert(t.clone(), idx);
            }
            let mut tasks = scc.clone();
            tasks.sort_by_key(|t| plan.tasks[t].seq);
            loops.push(Loop { tasks, reqs });
        }

        let mut in_edges: HashMap<TaskId, Vec<(TaskId, BTreeSet<ReqId>)>> = HashMap::new();
        for ((p, f), rs) in &edges {
            in_edges.entry(f.clone()).or_default().push((p.clone(), rs.clone()));
        }
        Derived { status, in_edges, reqs_from, loops, in_loop }
    }

    pub fn status(&self, id: &str) -> Status {
        self.status.get(id).copied().unwrap_or(Status::Todo)
    }

    /// Why a leaf cannot start, or None if it can. Checks loops, tentative
    /// requirements, then unfinished predecessors (own or inherited).
    pub fn blocks(&self, plan: &Plan, id: &str) -> Vec<Block> {
        let mut out = Vec::new();
        if let Some(i) = self.in_loop.get(id) {
            out.push(Block::Loop(*i));
        }
        for anc in plan.ancestors_and_self(id) {
            for rid in self.reqs_from.get(&anc.id).into_iter().flatten() {
                let r = &plan.reqs[rid];
                if r.conditional {
                    out.push(Block::Tentative(r.id.clone(), self.status(&r.on) == Status::Complete));
                }
            }
        }
        let mut per_req: BTreeMap<ReqId, usize> = BTreeMap::new();
        for (p, rs) in self.in_edges.get(id).into_iter().flatten() {
            if self.status(p) != Status::Complete && self.status(p) != Status::Dropped {
                for r in rs {
                    *per_req.entry(r.clone()).or_default() += 1;
                }
            }
        }
        for (r, n) in per_req {
            let on = plan.reqs[&r].on.clone();
            out.push(Block::Waiting(on, r, n));
        }
        out
    }

    pub fn is_ready(&self, plan: &Plan, id: &str) -> bool {
        !plan.is_parent(id)
            && matches!(self.status(id), Status::Todo | Status::Spike | Status::Paused)
            && self.blocks(plan, id).is_empty()
    }

    /// Leaves that can start now.
    pub fn ready(&self, plan: &Plan) -> Vec<TaskId> {
        let mut v: Vec<TaskId> = plan
            .tasks
            .keys()
            .filter(|id| self.is_ready(plan, id))
            .cloned()
            .collect();
        v.sort_by_key(|t| plan.tasks[t].seq);
        v
    }

    pub fn is_tentative(&self, plan: &Plan, id: &str) -> bool {
        plan.ancestors_and_self(id)
            .iter()
            .any(|a| self.reqs_from.get(&a.id).into_iter().flatten().any(|r| plan.reqs[r].conditional))
    }
}

fn status_of(plan: &Plan, leaf: &HashMap<TaskId, Status>, id: &str, memo: &mut HashMap<TaskId, Status>) -> Status {
    if let Some(s) = memo.get(id) {
        return *s;
    }
    let st = status_of_uncached(plan, leaf, id, memo);
    memo.insert(id.to_string(), st);
    st
}

fn status_of_uncached(plan: &Plan, leaf: &HashMap<TaskId, Status>, id: &str, memo: &mut HashMap<TaskId, Status>) -> Status {
    if let Some(s) = leaf.get(id) {
        if *s == Status::Dropped {
            return Status::Dropped;
        }
    }
    let kids = plan.children(id);
    if kids.is_empty() {
        return leaf.get(id).copied().unwrap_or(Status::Todo);
    }
    let ks: Vec<Status> = kids.iter().map(|k| status_of(plan, leaf, &k.id, memo)).collect();
    let live: Vec<Status> = ks.iter().copied().filter(|s| *s != Status::Dropped).collect();
    if live.is_empty() {
        return Status::Dropped;
    }
    if live.iter().all(|s| *s == Status::Complete) {
        return Status::Complete;
    }
    if live.iter().any(|s| matches!(s, Status::InProgress | Status::Paused | Status::Complete)) {
        return Status::InProgress;
    }
    Status::Todo
}

/// Tarjan's SCC over the given nodes.
fn tarjan<'a>(nodes: &'a [TaskId], adj: &HashMap<&'a TaskId, Vec<&'a TaskId>>) -> Vec<Vec<TaskId>> {
    struct St<'a> {
        index: HashMap<&'a TaskId, usize>,
        low: HashMap<&'a TaskId, usize>,
        on: HashSet<&'a TaskId>,
        stack: Vec<&'a TaskId>,
        next: usize,
        out: Vec<Vec<TaskId>>,
    }
    fn go<'a>(v: &'a TaskId, adj: &HashMap<&'a TaskId, Vec<&'a TaskId>>, st: &mut St<'a>) {
        st.index.insert(v, st.next);
        st.low.insert(v, st.next);
        st.next += 1;
        st.stack.push(v);
        st.on.insert(v);
        if let Some(ws) = adj.get(v) {
            for &w in ws {
                if !st.index.contains_key(w) {
                    go(w, adj, st);
                    let lw = st.low[w];
                    let lv = st.low.get_mut(v).unwrap();
                    *lv = (*lv).min(lw);
                } else if st.on.contains(w) {
                    let iw = st.index[w];
                    let lv = st.low.get_mut(v).unwrap();
                    *lv = (*lv).min(iw);
                }
            }
        }
        if st.low[v] == st.index[v] {
            let mut comp = Vec::new();
            loop {
                let w = st.stack.pop().unwrap();
                st.on.remove(w);
                comp.push(w.clone());
                if w == v {
                    break;
                }
            }
            st.out.push(comp);
        }
    }
    let mut st = St { index: HashMap::new(), low: HashMap::new(), on: HashSet::new(), stack: Vec::new(), next: 0, out: Vec::new() };
    for n in nodes {
        if !st.index.contains_key(n) {
            go(n, adj, &mut st);
        }
    }
    st.out
}
