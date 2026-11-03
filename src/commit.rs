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

/// Write the event. The caller holds the store lock (see `commands::open_rw`),
/// so HEAD cannot move between reading the parent and setting the child.
pub fn write(store: &Store, plan: &Plan, c: Commit) -> Result<Hash> {
    let snap_hash = plan.save(store)?;
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
        replayed_from: None,
    };
    let hash = store.write_object(ObjectType::Event, &ev)?;
    // The log must end with the event HEAD names. A store from before the
    // log existed, or one whose log git merged as a union, gets its log
    // rewritten from history once.
    let in_step = match (store.last_logged(), store.head()?) {
        (Some(last), Some(head)) => head.starts_with(&last),
        (None, None) => true,
        _ => false,
    };
    if !in_step {
        let history = store.history()?;
        store.rebuild_log(&history, plan)?;
    }
    store.move_head(&hash, &format!("{}: {}", ev.kind.name(), ev.message))?;
    store.append_log(&log_line(&hash, &ev, plan))?;
    // Carry the status memo forward when the previous one is at hand.
    if let Some(mut leaf) = ev.parent.as_deref().and_then(|p| store.cache_read::<std::collections::HashMap<TaskId, crate::state::Status>>("status", p)) {
        crate::state::apply_event(&mut leaf, &ev);
        store.cache_write("status", &hash, &leaf);
    }
    Ok(hash)
}

/// One line of `.tasks/log`: time, event, kind, message, the tasks by
/// outline number, the author. Newlines in the message are flattened.
pub fn log_line(hash: &str, ev: &Event, plan: &Plan) -> String {
    let subj = if ev.subjects.is_empty() {
        String::new()
    } else {
        format!("  [{}]", ev.subjects.iter().map(|s| plan.number_of(s)).collect::<Vec<_>>().join(" "))
    };
    format!(
        "{}  {}  {:<16} {}{}  -- {}{}",
        ev.time,
        &hash[..10],
        ev.kind.name(),
        ev.message.replace(['\n', '\r'], " "),
        subj,
        ev.author,
        if ev.forced { "  FORCED" } else { "" }
    )
}
