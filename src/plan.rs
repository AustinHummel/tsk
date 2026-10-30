//! The plan as loaded from a snapshot: every task, requirement and type in
//! memory, with the outline (derived numbering) and ref resolution.

use crate::model::*;
use crate::store::Store;
use anyhow::{bail, Result};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub tasks: BTreeMap<TaskId, Task>,
    pub reqs: BTreeMap<ReqId, Requirement>,
    pub types: BTreeMap<TypeId, TaskType>,
    pub next_task: u64,
    pub next_req: u64,
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

    /// Write every object and return the snapshot. Objects already stored
    /// are not rewritten, so this is cheap.
    pub fn save(&self, store: &Store) -> Result<Snapshot> {
        let mut snap = Snapshot {
            next_task: self.next_task,
            next_req: self.next_req,
            ..Default::default()
        };
        for (id, t) in &self.tasks {
            snap.tasks.insert(id.clone(), store.write_object(ObjectType::Task, t)?);
        }
        for (id, r) in &self.reqs {
            snap.reqs.insert(id.clone(), store.write_object(ObjectType::Req, r)?);
        }
        for (id, t) in &self.types {
            snap.types.insert(id.clone(), store.write_object(ObjectType::Type, t)?);
        }
        Ok(snap)
    }

    pub fn new_task_id(&mut self) -> TaskId {
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

    /// Children in creation order.
    pub fn children(&self, id: &str) -> Vec<&Task> {
        let mut v: Vec<&Task> = self
            .tasks
            .values()
            .filter(|t| t.parent.as_deref() == Some(id))
            .collect();
        v.sort_by_key(|t| t.seq);
        v
    }

    pub fn roots(&self) -> Vec<&Task> {
        let mut v: Vec<&Task> = self.tasks.values().filter(|t| t.parent.is_none()).collect();
        v.sort_by_key(|t| t.seq);
        v
    }

    pub fn is_parent(&self, id: &str) -> bool {
        self.tasks.values().any(|t| t.parent.as_deref() == Some(id))
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
        let mut out = vec![id.to_string()];
        let mut i = 0;
        while i < out.len() {
            let cur = out[i].clone();
            for c in self.children(&cur) {
                out.push(c.id.clone());
            }
            i += 1;
        }
        out
    }

    // ----- outline numbers -----------------------------------------------

    /// `1.2.4` for every task.
    pub fn numbering(&self) -> HashMap<TaskId, String> {
        let mut out = HashMap::new();
        fn walk(plan: &Plan, parent: Option<&str>, prefix: &str, out: &mut HashMap<TaskId, String>) {
            let kids = match parent {
                None => plan.roots(),
                Some(p) => plan.children(p),
            };
            for (i, k) in kids.iter().enumerate() {
                let num = if prefix.is_empty() {
                    format!("{}", i + 1)
                } else {
                    format!("{}.{}", prefix, i + 1)
                };
                out.insert(k.id.clone(), num.clone());
                walk(plan, Some(&k.id), &num, out);
            }
        }
        walk(self, None, "", &mut out);
        out
    }

    pub fn number_of(&self, id: &str) -> String {
        self.numbering().get(id).cloned().unwrap_or_else(|| "?".to_string())
    }

    /// Accept a task id (`t7`) or an outline number (`1.2.4`).
    pub fn resolve(&self, r: &str) -> Result<TaskId> {
        let r = r.trim();
        if self.tasks.contains_key(r) {
            return Ok(r.to_string());
        }
        if r.chars().all(|c| c.is_ascii_digit() || c == '.') && !r.is_empty() {
            let numbering = self.numbering();
            if let Some((id, _)) = numbering.iter().find(|(_, n)| n.as_str() == r) {
                return Ok(id.clone());
            }
            bail!("no task at outline position {}", r);
        }
        bail!("no task {}", r)
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

    pub fn requirer_label(&self, r: &Requirer) -> String {
        match r {
            Requirer::Task(t) => self.label(t),
            Requirer::Person(p) => p.clone(),
        }
    }
}
