//! The objects tsk stores. Every one is immutable once written; a change is
//! a new object and a new event pointing at a new snapshot.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type Hash = String;
pub type TaskId = String;
pub type ReqId = String;
pub type TypeId = String;

/// User-defined `key = value` pairs on a task or a requirement.
pub type Fields = BTreeMap<String, String>;

/// A task card. The tool owns the header fields; the human owns `body`,
/// `docs` and `fields`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Task {
    pub id: TaskId,
    pub title: String,
    pub parent: Option<TaskId>,
    /// Creation sequence; children are listed in this order and the outline
    /// number is derived from it. Fixed for life.
    pub seq: u64,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub types: Vec<TypeId>,
    /// Set when this task was created from a type item (type id, item id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_item: Option<(TypeId, String)>,
    /// Set on the `core` child made when a type is applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_of: Option<TaskId>,
    /// How the children together fulfill this task. Set by breakdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narrative: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
    /// Paths or URLs of the documents this task serves or produced.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub docs: Vec<String>,
    /// User-defined fields.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: Fields,
}

/// Who or what requires something of a task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "id", rename_all = "lowercase")]
pub enum Requirer {
    Task(TaskId),
    Person(String),
}

/// One record for three uses: a dependency link (`blocking`), a technical
/// requirement (not blocking), and a "required by <person>" entry.
///
/// `on` is the task the requirement sits on. For a dependency, `on` is the
/// predecessor and `from` the successor: the successor requires the reason
/// of the predecessor, and cannot start until the predecessor is complete.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Requirement {
    pub id: ReqId,
    pub from: Requirer,
    pub on: TaskId,
    pub reason: String,
    pub blocking: bool,
    /// The requiring task's existence, not just its start, waits on `on`.
    #[serde(default)]
    pub conditional: bool,
    pub created: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_item: Option<(TypeId, String)>,
    /// User-defined fields.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: Fields,
}

/// A requirement declared on a type item, resolved when the type is applied.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ItemReq {
    /// `core`, `item:<item id>`, or a task ref.
    pub on: String,
    pub reason: String,
    pub blocking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TypeItem {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// The task ref this item's existence waits on, if it is tentative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<String>,
    #[serde(default)]
    pub reqs: Vec<ItemReq>,
    /// When this item applies, for the agent to confirm or drop it per
    /// instance; shown on every instance's card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// Documents this item's scope comes from (a process, a skill); every
    /// instance inherits them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub docs: Vec<String>,
}

/// A reusable slice of scope stamped onto every task that carries it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskType {
    pub id: TypeId,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub items: Vec<TypeItem>,
    /// The documents the type stands for (a process, a skill); every
    /// instance inherits them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub docs: Vec<String>,
}

/// The whole plan at one moment, flat: every object by id, plus the
/// counters. This is the in-memory form; on disk a snapshot is stored as a
/// tree (`SnapshotHeader` + `Tree` objects) so that a change rewrites one
/// path of the plan, not all of it. Stores written before 0.2 hold this
/// flat form directly, and still load.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Snapshot {
    pub tasks: BTreeMap<TaskId, Hash>,
    pub reqs: BTreeMap<ReqId, Hash>,
    pub types: BTreeMap<TypeId, Hash>,
    pub next_task: u64,
    pub next_req: u64,
}

/// The on-disk snapshot since 0.2: the root tree, the types, the counters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotHeader {
    pub version: u32,
    /// The tree holding the root tasks; absent when the plan is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<Hash>,
    #[serde(default)]
    pub types: BTreeMap<TypeId, Hash>,
    pub next_task: u64,
    pub next_req: u64,
}

/// One level of the plan: the children of one parent (or the roots), in
/// creation order, each with its task object, the requirements sitting on
/// it, and the tree of its own children.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tree {
    pub entries: Vec<TreeEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TreeEntry {
    pub id: TaskId,
    pub task: Hash,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reqs: BTreeMap<ReqId, Hash>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub children: Option<Hash>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum EventKind {
    Init,
    Created,
    Edited,
    BrokenDown,
    Amended,
    Linked,
    Required,
    Moved,
    Unlinked,
    TypeAdded,
    TypeEdited,
    Applied,
    FlaggedAsSpike,
    Confirmed,
    Started,
    Paused,
    Completed,
    Uncompleted,
    Dropped,
    /// Fields or docs changed on a task or requirement.
    Set,
    /// Old snapshots were removed; the plan is unchanged.
    Pruned,
    /// Another line of history was replayed onto this one.
    Merged,
}

impl EventKind {
    pub fn name(self) -> &'static str {
        match self {
            EventKind::Init => "init",
            EventKind::Created => "created",
            EventKind::Edited => "edited",
            EventKind::BrokenDown => "broken-down",
            EventKind::Amended => "amended",
            EventKind::Linked => "linked",
            EventKind::Required => "required",
            EventKind::Moved => "moved",
            EventKind::Unlinked => "unlinked",
            EventKind::TypeAdded => "type-added",
            EventKind::TypeEdited => "type-edited",
            EventKind::Applied => "applied",
            EventKind::FlaggedAsSpike => "flagged-as-spike",
            EventKind::Confirmed => "confirmed",
            EventKind::Started => "started",
            EventKind::Paused => "paused",
            EventKind::Completed => "completed",
            EventKind::Uncompleted => "uncompleted",
            EventKind::Dropped => "dropped",
            EventKind::Set => "set",
            EventKind::Pruned => "pruned",
            EventKind::Merged => "merged",
        }
    }
}

/// A commit: the snapshot after the change, the event before it, who, when,
/// what kind, which tasks it touched.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Event {
    pub kind: EventKind,
    pub parent: Option<Hash>,
    pub snapshot: Hash,
    pub time: String,
    pub author: String,
    pub message: String,
    #[serde(default)]
    pub subjects: Vec<TaskId>,
    /// Tasks started or completed together as one group (the loop shortcut).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub group: Vec<TaskId>,
    #[serde(default)]
    pub forced: bool,
    /// When this event is the replay of an event from another line of
    /// history (a merge), the original's hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replayed_from: Option<Hash>,
}

/// What an object is, for `cat-object`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectType {
    Task,
    Req,
    Type,
    Snapshot,
    Tree,
    Event,
}

impl ObjectType {
    pub fn name(self) -> &'static str {
        match self {
            ObjectType::Task => "task",
            ObjectType::Req => "req",
            ObjectType::Type => "type",
            ObjectType::Snapshot => "snapshot",
            ObjectType::Tree => "tree",
            ObjectType::Event => "event",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "task" => ObjectType::Task,
            "req" => ObjectType::Req,
            "type" => ObjectType::Type,
            "snapshot" => ObjectType::Snapshot,
            "tree" => ObjectType::Tree,
            "event" => ObjectType::Event,
            _ => return None,
        })
    }
}
