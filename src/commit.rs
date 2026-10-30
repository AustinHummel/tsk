//! Writing an event: save the plan as a snapshot, write the event pointing
//! at it and at the previous HEAD, move HEAD. One call per porcelain command,
//! so every command is atomic.

use crate::model::*;
use crate::plan::Plan;
use crate::store::Store;
use anyhow::Result;

pub struct Commit {
    pub kind: EventKind,
    pub message: String,
    pub subjects: Vec<TaskId>,
    pub group: Vec<TaskId>,
    pub forced: bool,
}

impl Commit {
    pub fn new(kind: EventKind, message: impl Into<String>, subjects: Vec<TaskId>) -> Commit {
        Commit { kind, message: message.into(), subjects, group: Vec::new(), forced: false }
    }
}

/// UTC, RFC 3339, to the second. Computed from the epoch so the tool needs
/// no platform time library.
pub fn now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, m, d, rem / 3600, (rem % 3600) / 60, rem % 60)
}

/// Days since 1970-01-01 to (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn write(store: &Store, plan: &Plan, c: Commit) -> Result<Hash> {
    let snap = plan.save(store)?;
    let snap_hash = store.write_object(ObjectType::Snapshot, &snap)?;
    let ev = Event {
        kind: c.kind,
        parent: store.head()?,
        snapshot: snap_hash,
        time: now(),
        author: store.author(),
        message: c.message,
        subjects: c.subjects,
        group: c.group,
        forced: c.forced,
    };
    let hash = store.write_object(ObjectType::Event, &ev)?;
    store.set_head(&hash)?;
    Ok(hash)
}
