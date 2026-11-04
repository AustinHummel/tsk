//! The plan as loaded from a snapshot: every task, requirement and type in
//! memory, with the outline (derived numbering) and ref resolution.

use crate::model::*;
use crate::store::Store;
use anyhow::{bail, Result};
use std::cell::{Ref, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Plan {
    pub tasks: BTreeMap<TaskId, Task>,
    pub reqs: BTreeMap<ReqId, Requirement>,
    pub types: BTreeMap<TypeId, TaskType>,
    pub next_task: u64,
    pub next_req: u64,
    /// The tree shape and outline numbers, built once per plan state. Any
    /// code that adds a task or moves one calls `invalidate`.
    #[serde(skip)]
    index: RefCell<Option<Index>>,
}

/// Everything about the tree that is derived from `parent`, `seq` and the
/// positions adopted tasks left behind.
#[derive(Debug, Clone, Default)]
struct Index {
    children: HashMap<Option<TaskId>, Vec<TaskId>>,
    /// Every slot under a parent in order: (seq, task, whether the task is
    /// still there). A slot a task vacated by adoption keeps its number.
    slots: HashMap<Option<TaskId>, Vec<(u64, TaskId, bool)>>,
    numbering: HashMap<TaskId, String>,
    parents: HashSet<TaskId>,
    /// Outline numbers left vacant by adoption: the number, and the task
    /// that held it.
    vacated: HashMap<String, TaskId>,
}

impl Plan {
    pub fn load(store: &Store, snap: &Snapshot) -> Result<Plan> {
        let mut plan = Plan {
            next_task: snap.next_task,
            next_req: snap.next_req,
            ..Default::default()
        };
        for (id, h) in &snap.tasks {
            plan.tasks.insert(id.clone(), store.read_object(ObjectType::Task, h)?);
        }
        for (id, h) in &snap.reqs {
            plan.reqs.insert(id.clone(), store.read_object(ObjectType::Req, h)?);
        }
        for (id, h) in &snap.types {
            plan.types.insert(id.clone(), store.read_object(ObjectType::Type, h)?);
        }
        Ok(plan)
    }

    /// Write every object and the snapshot, as a tree of per-parent objects,
    /// and return the snapshot's hash. Objects already stored are not
    /// rewritten, so a change costs one path of the tree, not the plan.
    pub fn save(&self, store: &Store) -> Result<Hash> {
        let mut reqs_on: HashMap<&str, BTreeMap<ReqId, Hash>> = HashMap::new();
        for (id, r) in &self.reqs {
            reqs_on.entry(r.on.as_str()).or_default().insert(id.clone(), store.write_object(ObjectType::Req, r)?);
        }
        fn write_tree(plan: &Plan, store: &Store, parent: Option<&str>, reqs_on: &HashMap<&str, BTreeMap<ReqId, Hash>>) -> Result<Option<Hash>> {
            let kids = match parent {
                None => plan.roots(),
                Some(p) => plan.children(p),
            };
            if kids.is_empty() {
                return Ok(None);
            }
            let mut tree = Tree::default();
            for k in kids {
                tree.entries.push(TreeEntry {
                    id: k.id.clone(),
                    task: store.write_object(ObjectType::Task, k)?,
                    reqs: reqs_on.get(k.id.as_str()).cloned().unwrap_or_default(),
                    children: write_tree(plan, store, Some(&k.id), reqs_on)?,
                });
            }
            Ok(Some(store.write_object(ObjectType::Tree, &tree)?))
        }
        let mut types = BTreeMap::new();
        for (id, t) in &self.types {
            types.insert(id.clone(), store.write_object(ObjectType::Type, t)?);
        }
        let header = SnapshotHeader { version: 2, tree: write_tree(self, store, None, &reqs_on)?, types, next_task: self.next_task, next_req: self.next_req };
        let hash = store.write_object(ObjectType::Snapshot, &header)?;
        store.cache_write("plan", &hash, self);
        Ok(hash)
    }

    pub fn new_task_id(&mut self) -> TaskId {
        self.invalidate();
        self.next_task += 1;
        format!("t{}", self.next_task)
    }

    pub fn new_req_id(&mut self) -> ReqId {
        self.next_req += 1;
        format!("r{}", self.next_req)
    }

    // ----- structure -----------------------------------------------------

    pub fn task(&self, id: &str) -> Result<&Task> {
        self.tasks.get(id).ok_or_else(|| anyhow::anyhow!("no task {}", id))
    }

    /// Drop the cached tree shape; the next structural query rebuilds it.
    pub fn invalidate(&self) {
        *self.index.borrow_mut() = None;
    }

    fn index(&self) -> Ref<'_, Index> {
        if self.index.borrow().is_none() {
            let mut ix = Index::default();
            let mut by_parent: HashMap<Option<TaskId>, Vec<(u64, TaskId, bool)>> = HashMap::new();
            for t in self.tasks.values() {
                by_parent.entry(t.parent.clone()).or_default().push((t.seq, t.id.clone(), true));
                for f in &t.former {
                    by_parent.entry(f.parent.clone()).or_default().push((f.seq, t.id.clone(), false));
                }
            }
            for (p, mut slots) in by_parent {
                slots.sort_by_key(|(s, id, _)| (*s, id.clone()));
                let kids: Vec<TaskId> = slots.iter().filter(|(_, _, here)| *here).map(|(_, id, _)| id.clone()).collect();
                if let (Some(p), false) = (&p, kids.is_empty()) {
                    ix.parents.insert(p.clone());
                }
                ix.children.insert(p.clone(), kids);
                ix.slots.insert(p, slots);
            }
            fn walk(ix: &mut Index, parent: Option<&str>, prefix: &str) {
                let slots = ix.slots.get(&parent.map(String::from)).cloned().unwrap_or_default();
                for (i, (_, id, here)) in slots.iter().enumerate() {
                    let num = if prefix.is_empty() { format!("{}", i + 1) } else { format!("{}.{}", prefix, i + 1) };
                    if *here {
                        ix.numbering.insert(id.clone(), num.clone());
                        walk(ix, Some(id), &num);
                    } else {
                        ix.vacated.insert(num, id.clone());
                    }
                }
            }
            walk(&mut ix, None, "");
            *self.index.borrow_mut() = Some(ix);
        }
        Ref::map(self.index.borrow(), |o| o.as_ref().unwrap())
    }

    /// Children in creation order.
    pub fn children(&self, id: &str) -> Vec<&Task> {
        let ix = self.index();
        ix.children.get(&Some(id.to_string())).map(|v| v.iter().filter_map(|k| self.tasks.get(k)).collect()).unwrap_or_default()
    }

    pub fn roots(&self) -> Vec<&Task> {
        let ix = self.index();
        ix.children.get(&None).map(|v| v.iter().filter_map(|k| self.tasks.get(k)).collect()).unwrap_or_default()
    }

    pub fn is_parent(&self, id: &str) -> bool {
        self.index().parents.contains(id)
    }

    /// Root first, the task itself last.
    pub fn ancestors_and_self(&self, id: &str) -> Vec<&Task> {
        let mut chain = Vec::new();
        let mut cur = self.tasks.get(id);
        while let Some(t) = cur {
            chain.push(t);
            cur = t.parent.as_deref().and_then(|p| self.tasks.get(p));
        }
        chain.reverse();
        chain
    }

    /// The task and every descendant.
    pub fn subtree(&self, id: &str) -> Vec<TaskId> {
        let ix = self.index();
        let mut out = vec![id.to_string()];
        let mut i = 0;
        while i < out.len() {
            if let Some(kids) = ix.children.get(&Some(out[i].clone())) {
                out.extend(kids.iter().cloned());
            }
            i += 1;
        }
        out
    }

    // ----- outline numbers -----------------------------------------------

    /// `1.2.4` for every task.
    pub fn numbering(&self) -> HashMap<TaskId, String> {
        self.index().numbering.clone()
    }

    pub fn number_of(&self, id: &str) -> String {
        self.index().numbering.get(id).cloned().unwrap_or_else(|| "?".to_string())
    }

    /// Accept a task id (`t7`), an outline number (`1.2.4`), or an exact
    /// title when it names one task.
    pub fn resolve(&self, r: &str) -> Result<TaskId> {
        let r = r.trim();
        if r.is_empty() {
            bail!("empty task reference");
        }
        if self.tasks.contains_key(r) {
            return Ok(r.to_string());
        }
        if r.chars().all(|c| c.is_ascii_digit() || c == '.') {
            let numbering = self.numbering();
            if let Some((id, _)) = numbering.iter().find(|(_, n)| n.as_str() == r) {
                return Ok(id.clone());
            }
            // The number, or the position above it, may have been vacated
            // by an adoption: say where that task went.
            let moved = self.index().vacated.iter().find(|(n, _)| r == n.as_str() || r.starts_with(&format!("{}.", n))).map(|(n, id)| (n.clone(), id.clone()));
            match moved {
                Some((n, id)) => bail!("no task at outline position {}: {} held {} until it was adopted, and is now {}", r, id, n, self.number_of(&id)),
                None => bail!("no task at outline position {}", r),
            }
        }
        let lower = r.to_lowercase();
        let mut exact: Vec<&Task> = self.tasks.values().filter(|t| t.title.to_lowercase() == lower).collect();
        exact.sort_by_key(|t| t.seq);
        match exact.len() {
            1 => return Ok(exact[0].id.clone()),
            0 => {}
            _ => bail!(
                "`{}` names {} tasks; use an id or number:\n{}",
                r,
                exact.len(),
                exact.iter().map(|t| format!("  {}", self.label(&t.id))).collect::<Vec<_>>().join("\n")
            ),
        }
        let mut near: Vec<&Task> = self.tasks.values().filter(|t| t.title.to_lowercase().contains(&lower)).collect();
        near.sort_by_key(|t| t.seq);
        if near.is_empty() {
            bail!("no task `{}`; tasks are named by id (t7), number (1.2.4), or exact title", r);
        }
        bail!(
            "no task `{}`; did you mean:\n{}",
            r,
            near.iter().take(8).map(|t| format!("  {}", self.label(&t.id))).collect::<Vec<_>>().join("\n")
        )
    }

    pub fn label(&self, id: &str) -> String {
        match self.tasks.get(id) {
            Some(t) => format!("{} {} {}", self.number_of(id), id, t.title),
            None => id.to_string(),
        }
    }

    // ----- requirements --------------------------------------------------

    /// Requirements sitting on `id`.
    pub fn reqs_on(&self, id: &str) -> Vec<&Requirement> {
        let mut v: Vec<&Requirement> = self.reqs.values().filter(|r| r.on == id).collect();
        v.sort_by_key(|r| r.id.clone());
        v
    }

    /// Requirements `id` places on other tasks.
    pub fn reqs_from(&self, id: &str) -> Vec<&Requirement> {
        let mut v: Vec<&Requirement> = self
            .reqs
            .values()
            .filter(|r| matches!(&r.from, Requirer::Task(t) if t == id))
            .collect();
        v.sort_by_key(|r| r.id.clone());
        v
    }

    /// A field's value on the task or the nearest ancestor that sets it.
    pub fn field(&self, id: &str, key: &str) -> Option<(TaskId, String)> {
        let chain = self.ancestors_and_self(id);
        chain.iter().rev().find_map(|t| t.fields.get(key).map(|v| (t.id.clone(), v.clone())))
    }

    /// Whether the task matches every `key[=value]` filter, own or inherited.
    pub fn matches(&self, id: &str, filters: &[(String, Option<String>)]) -> bool {
        filters.iter().all(|(k, v)| match (self.field(id, k), v) {
            (Some(_), None) => true,
            (Some((_, have)), Some(want)) => have == *want,
            (None, _) => false,
        })
    }

    pub fn requirer_label(&self, r: &Requirer) -> String {
        match r {
            Requirer::Task(t) => self.label(t),
            Requirer::Person(p) => p.clone(),
        }
    }
}
