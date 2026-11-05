//! The object store: `.tasks/` at the project root, found by walking up or
//! named outright. Objects are plain UTF-8 text files named by the SHA-256
//! of their content, `<type>\n<json>`. `HEAD` holds the hash of the latest
//! event. `lock` serializes writers. `log` is the human-readable event log.

use crate::model::*;
use anyhow::{anyhow, bail, Context, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const DIR: &str = ".tasks";

/// What is ignored inside the store when it is committed with git.
const GITIGNORE: &str = "# transient files of a running tsk, its local reflog, and its disposable cache\nlock\nlock.broken-*\nlock.breaking-*\nHEAD.tmp*\nlog.tmp*\n*.EDIT\n*.tmp\nreflog\ncache/\nclaims/\n";

/// Objects are named by the hash of their bytes: git must never convert
/// their line endings. The log stays diffable.
pub const GITATTRIBUTES: &str = "* -text\nlog text eol=lf merge=union\nHEAD merge=tsk\n";

const LOG_HEADER: &str = "# tsk event log: one line per event, oldest first. time  event  kind  message  [tasks]  -- author\n";

pub struct Store {
    pub dir: PathBuf,
}

/// One move of HEAD.
#[derive(Debug, Clone)]
pub struct ReflogEntry {
    pub old: Option<Hash>,
    pub new: Hash,
    pub time: String,
    pub author: String,
    pub what: String,
}

/// Whether the process named in a lock file (`pid N`) is known to have
/// exited on this machine. False when it is alive, or when that cannot be
/// told (no pid line, a lock from another machine, no way to ask).
fn lock_holder_exited(holder: &str) -> bool {
    let Some(pid) = holder.lines().find_map(|l| l.strip_prefix("pid ")).and_then(|p| p.trim().parse::<u32>().ok()) else { return false };
    if pid == std::process::id() {
        return false;
    }
    #[cfg(windows)]
    {
        let out = std::process::Command::new("tasklist").args(["/FI", &format!("PID eq {}", pid), "/NH", "/FO", "CSV"]).output();
        match out {
            Ok(o) if o.status.success() => !String::from_utf8_lossy(&o.stdout).contains(&format!("\"{}\"", pid)),
            _ => false,
        }
    }
    #[cfg(not(windows))]
    {
        match std::process::Command::new("kill").args(["-0", &pid.to_string()]).output() {
            Ok(o) => !o.status.success(),
            Err(_) => false,
        }
    }
}

/// What `.tasks/HEAD` says.
#[derive(Debug, Default, Clone)]
pub struct HeadFile {
    /// Our latest event.
    pub ours: Option<Hash>,
    /// A line of history still to merge into ours (`merge <hash>`).
    pub pending: Option<Hash>,
    /// Git left conflict markers: this is the other side's HEAD.
    pub conflicted: bool,
    pub theirs: Option<Hash>,
    pub theirs_pending: Option<Hash>,
}

impl HeadFile {
    /// Whatever other line of history is waiting to be merged, however it
    /// got here.
    pub fn to_merge(&self) -> Option<Hash> {
        self.pending.clone().or_else(|| self.theirs.clone())
    }
}

/// Holds `.tasks/lock` until dropped. The token tells this lock from any
/// other process's: the file is removed on drop only if it is still ours.
pub struct Lock {
    path: PathBuf,
    token: String,
}

impl Lock {
    /// Whether `.tasks/lock` still carries our token.
    pub fn held(&self) -> bool {
        lock_token(&fs::read_to_string(&self.path).unwrap_or_default()).as_deref() == Some(self.token.as_str())
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        if self.held() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn lock_token(content: &str) -> Option<String> {
    content.lines().find_map(|l| l.strip_prefix("token ")).map(|t| t.trim().to_string())
}

/// Another process committed on top of the HEAD this command read, so its
/// change was not written. The command is safe to run again.
#[derive(Debug)]
pub struct StoreMoved(pub String);

impl std::fmt::Display for StoreMoved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the store moved while this command ran: another process committed on top of {}; nothing was written. Run it again.", &self.0[..10.min(self.0.len())])
    }
}

impl std::error::Error for StoreMoved {}

/// The store format this binary writes, declared as `format` in
/// `.tasks/config`. Stores from before 0.5.0 carry no line and count as 0;
/// every older form is still read (the snapshot header has its own
/// version). The number moves only when something on disk changes shape:
/// 1 (0.5.0) added the `adopted` event kind and a task's former positions;
/// 2 (0.6.0) added the `closed` and `reopened` event kinds and a task's
/// `fit`, `ongoing` and `after`.
pub const FORMAT: u32 = 2;

impl Store {
    pub fn init(at: &Path) -> Result<Store> {
        let dir = at.join(DIR);
        if dir.exists() {
            bail!("{} already exists", dir.display());
        }
        fs::create_dir_all(dir.join("objects"))?;
        let store = Store { dir };
        let mut cfg = BTreeMap::new();
        cfg.insert("format".to_string(), FORMAT.to_string());
        store.write_config(&cfg)?;
        fs::write(store.dir.join(".gitignore"), GITIGNORE)?;
        fs::write(store.dir.join(".gitattributes"), GITATTRIBUTES)?;
        fs::write(store.dir.join("log"), LOG_HEADER)?;
        Ok(store)
    }

    /// Find the store by walking up from `start`.
    pub fn discover(start: &Path) -> Result<Store> {
        let mut cur = Some(start.to_path_buf());
        while let Some(p) = cur {
            let dir = p.join(DIR);
            if dir.is_dir() {
                return Ok(Store { dir });
            }
            cur = p.parent().map(Path::to_path_buf);
        }
        bail!("not a tsk project (no {} found here or above); run `tsk init`, or name the store with --store / TSK_STORE", DIR)
    }

    /// Open the store a path names: the `.tasks` directory itself, or the
    /// project directory that holds one.
    pub fn at(path: &Path) -> Result<Store> {
        let p = if path.file_name().map(|n| n == DIR).unwrap_or(false) { path.to_path_buf() } else { path.join(DIR) };
        if !p.is_dir() {
            bail!("no store at {} (looked for {})", path.display(), p.display());
        }
        Ok(Store { dir: p })
    }

    /// The project directory: the parent of `.tasks`.
    pub fn root(&self) -> PathBuf {
        self.dir.parent().map(Path::to_path_buf).unwrap_or_else(|| self.dir.clone())
    }

    // ----- objects -------------------------------------------------------

    pub fn hash_bytes(bytes: &[u8]) -> Hash {
        hex::encode(Sha256::digest(bytes))
    }

    fn object_path(&self, hash: &str) -> PathBuf {
        self.dir.join("objects").join(&hash[..2]).join(&hash[2..])
    }

    /// Write an object. Written to a temporary name and renamed, so a
    /// parallel reader never sees a partial object; an object that already
    /// exists has, by construction, the same content and is left alone.
    pub fn write_object<T: Serialize>(&self, ty: ObjectType, value: &T) -> Result<Hash> {
        let json = serde_json::to_string_pretty(value)?;
        let text = format!("{}\n{}\n", ty.name(), json);
        let hash = Self::hash_bytes(text.as_bytes());
        let path = self.object_path(&hash);
        if !path.exists() {
            fs::create_dir_all(path.parent().unwrap())?;
            let tmp = path.with_extension(format!("tmp{}", std::process::id()));
            fs::write(&tmp, text)?;
            if let Err(e) = fs::rename(&tmp, &path) {
                let _ = fs::remove_file(&tmp);
                if !path.exists() {
                    return Err(e).with_context(|| format!("writing object {}", hash));
                }
            }
        }
        Ok(hash)
    }

    pub fn read_raw(&self, hash: &str) -> Result<(ObjectType, String)> {
        let full = self.resolve_hash(hash)?;
        let text = fs::read_to_string(self.object_path(&full))
            .with_context(|| format!("object {} not found (pruned, or not yet committed to this copy)", hash))?;
        let (ty, body) = text
            .split_once('\n')
            .ok_or_else(|| anyhow!("object {} is malformed", hash))?;
        let ty = ObjectType::parse(ty.trim_end()).ok_or_else(|| anyhow!("object {} has unknown type {}", hash, ty))?;
        Ok((ty, body.to_string()))
    }

    pub fn read_object<T: DeserializeOwned>(&self, ty: ObjectType, hash: &str) -> Result<T> {
        let (actual, body) = self.read_raw(hash)?;
        if actual != ty {
            bail!("object {} is a {}, not a {}", hash, actual.name(), ty.name());
        }
        Ok(serde_json::from_str(&body)?)
    }

    pub fn has_object(&self, hash: &str) -> bool {
        hash.len() == 64 && self.object_path(hash).is_file()
    }

    /// Every object hash in the store, with its size in bytes.
    pub fn all_objects(&self) -> Result<Vec<(Hash, u64)>> {
        let mut out = Vec::new();
        let objs = self.dir.join("objects");
        if !objs.is_dir() {
            return Ok(out);
        }
        for d in fs::read_dir(&objs)? {
            let d = d?;
            let prefix = d.file_name().to_string_lossy().to_string();
            if prefix.len() != 2 || !d.path().is_dir() {
                continue;
            }
            for f in fs::read_dir(d.path())? {
                let f = f?;
                let name = f.file_name().to_string_lossy().to_string();
                if name.len() != 62 {
                    continue; // a temp file of a writer in flight
                }
                out.push((format!("{}{}", prefix, name), f.metadata()?.len()));
            }
        }
        Ok(out)
    }

    pub fn remove_object(&self, hash: &str) -> Result<()> {
        fs::remove_file(self.object_path(hash)).with_context(|| format!("removing object {}", hash))
    }

    /// Expand an abbreviated hash to the full one.
    pub fn resolve_hash(&self, prefix: &str) -> Result<Hash> {
        if prefix.len() == 64 {
            return Ok(prefix.to_string());
        }
        if prefix.len() < 4 {
            bail!("hash prefix {} is too short", prefix);
        }
        let dir = self.dir.join("objects").join(&prefix[..2]);
        let mut found = Vec::new();
        if dir.is_dir() {
            for e in fs::read_dir(dir)? {
                let name = e?.file_name().to_string_lossy().to_string();
                if name.len() == 62 && name.starts_with(&prefix[2..]) {
                    found.push(format!("{}{}", &prefix[..2], name));
                }
            }
        }
        match found.len() {
            0 => bail!("no object matches {}", prefix),
            1 => Ok(found.remove(0)),
            _ => bail!("{} is ambiguous", prefix),
        }
    }

    // ----- snapshots -----------------------------------------------------

    /// Read a snapshot in either on-disk form into the flat form.
    pub fn read_snapshot(&self, hash: &str) -> Result<Snapshot> {
        if !self.has_object(hash) {
            bail!("its snapshot was pruned, so the plan as of then cannot be read (the event and its log line remain)");
        }
        let (ty, body) = self.read_raw(hash)?;
        if ty != ObjectType::Snapshot {
            bail!("object {} is a {}, not a snapshot", hash, ty.name());
        }
        let v: serde_json::Value = serde_json::from_str(&body)?;
        if v.get("version").is_none() {
            return Ok(serde_json::from_value(v)?);
        }
        let h: SnapshotHeader = serde_json::from_value(v)?;
        let mut snap = Snapshot { types: h.types, next_task: h.next_task, next_req: h.next_req, ..Default::default() };
        if let Some(t) = &h.tree {
            self.expand_tree(t, &mut snap)?;
        }
        Ok(snap)
    }

    fn expand_tree(&self, hash: &str, snap: &mut Snapshot) -> Result<()> {
        let tree: Tree = self.read_object(ObjectType::Tree, hash)?;
        for e in tree.entries {
            snap.tasks.insert(e.id, e.task);
            for (rid, rh) in e.reqs {
                snap.reqs.insert(rid, rh);
            }
            if let Some(c) = &e.children {
                self.expand_tree(c, snap)?;
            }
        }
        Ok(())
    }

    /// Every object a snapshot reaches, itself included, for `prune`.
    pub fn snapshot_closure(&self, hash: &str, out: &mut BTreeSet<Hash>) -> Result<()> {
        if !out.insert(hash.to_string()) {
            return Ok(());
        }
        let (_, body) = self.read_raw(hash)?;
        let v: serde_json::Value = serde_json::from_str(&body)?;
        if v.get("version").is_none() {
            let s: Snapshot = serde_json::from_value(v)?;
            out.extend(s.tasks.into_values());
            out.extend(s.reqs.into_values());
            out.extend(s.types.into_values());
            return Ok(());
        }
        let h: SnapshotHeader = serde_json::from_value(v)?;
        out.extend(h.types.into_values());
        let mut stack: Vec<Hash> = h.tree.into_iter().collect();
        while let Some(t) = stack.pop() {
            if !out.insert(t.clone()) {
                continue;
            }
            let tree: Tree = self.read_object(ObjectType::Tree, &t)?;
            for e in tree.entries {
                out.insert(e.task);
                out.extend(e.reqs.into_values());
                if let Some(c) = e.children {
                    stack.push(c);
                }
            }
        }
        Ok(())
    }

    // ----- cache ---------------------------------------------------------
    //
    // `.tasks/cache/` holds memos of pure functions of the store, keyed by
    // the content hash they derive from, so they can never be stale: the
    // plan a snapshot loads to, and the leaf statuses a HEAD implies. It is
    // disposable (delete it any time), never committed, and rebuilt on a
    // miss. It is what keeps a command from reading every event and every
    // object of a large store.

    fn cache_path(&self, kind: &str, key: &str) -> PathBuf {
        self.dir.join("cache").join(format!("{}-{}.json", kind, key))
    }

    pub fn cache_read<T: DeserializeOwned>(&self, kind: &str, key: &str) -> Option<T> {
        let text = fs::read_to_string(self.cache_path(kind, key)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Write a memo, replacing older memos of the same kind. Failures are
    /// ignored: a cache that cannot be written only costs time.
    pub fn cache_write<T: Serialize>(&self, kind: &str, key: &str, value: &T) {
        let dir = self.dir.join("cache");
        if fs::create_dir_all(&dir).is_err() {
            return;
        }
        self.ensure_ignored("cache/");
        let Ok(text) = serde_json::to_string(value) else { return };
        let path = self.cache_path(kind, key);
        let tmp = dir.join(format!("{}-{}.tmp{}", kind, key, std::process::id()));
        if fs::write(&tmp, text).is_ok() && fs::rename(&tmp, &path).is_err() {
            let _ = fs::remove_file(&tmp);
        }
        if let Ok(rd) = fs::read_dir(&dir) {
            let prefix = format!("{}-", kind);
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with(&prefix) && name.ends_with(".json") && e.path() != path {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
    }

    /// A store from before the cache existed gets the ignore line, so the
    /// cache is never committed.
    fn ensure_ignored(&self, line: &str) {
        let p = self.dir.join(".gitignore");
        let cur = fs::read_to_string(&p).unwrap_or_default();
        if !cur.lines().any(|l| l.trim() == line) {
            let _ = fs::write(&p, format!("{}{}{}\n", cur, if cur.is_empty() || cur.ends_with('\n') { "" } else { "\n" }, line));
        }
    }

    /// The plan a snapshot loads to, from the cache or the objects.
    pub fn load_plan(&self, snapshot: &str) -> Result<crate::plan::Plan> {
        if let Some(p) = self.cache_read::<crate::plan::Plan>("plan", snapshot) {
            return Ok(p);
        }
        let snap = self.read_snapshot(snapshot)?;
        let plan = crate::plan::Plan::load(self, &snap)?;
        self.cache_write("plan", snapshot, &plan);
        Ok(plan)
    }

    /// The per-leaf statuses an event implies, from the cache or the
    /// history back from it.
    pub fn leaf_statuses_at(&self, head: &str) -> Result<HashMap<TaskId, crate::state::Status>> {
        if let Some(m) = self.cache_read::<HashMap<TaskId, crate::state::Status>>("status", head) {
            return Ok(m);
        }
        let history = self.history_from(Some(head.to_string()))?;
        let m = crate::state::leaf_statuses(&history);
        self.cache_write("status", head, &m);
        Ok(m)
    }

    // ----- HEAD ----------------------------------------------------------

    /// The HEAD file, parsed. Normally one line, the latest event. It may
    /// carry a second line `merge <hash>`: another line of history to replay
    /// (written by the git merge driver). Or git may have left conflict
    /// markers in it, with ours above and theirs below.
    pub fn read_head_file(&self) -> Result<HeadFile> {
        let p = self.dir.join("HEAD");
        if !p.exists() {
            return Ok(HeadFile::default());
        }
        let text = fs::read_to_string(p)?;
        let mut hf = HeadFile::default();
        let mut side = 0; // 0 ours (or no conflict), 1 theirs
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with("<<<<<<<") {
                side = 0;
                hf.conflicted = true;
                continue;
            }
            if line.starts_with("=======") && hf.conflicted {
                side = 1;
                continue;
            }
            if line.starts_with(">>>>>>>") {
                side = 2;
                continue;
            }
            if line.is_empty() || side == 2 {
                continue;
            }
            if let Some(h) = line.strip_prefix("merge ") {
                if side == 0 {
                    hf.pending = Some(h.trim().to_string());
                } else {
                    hf.theirs_pending = Some(h.trim().to_string());
                }
            } else if side == 0 && hf.ours.is_none() {
                hf.ours = Some(line.to_string());
            } else if side == 1 && hf.theirs.is_none() {
                hf.theirs = Some(line.to_string());
            }
        }
        Ok(hf)
    }

    /// The latest event on our line of history.
    pub fn head(&self) -> Result<Option<Hash>> {
        Ok(self.read_head_file()?.ours)
    }

    /// Write HEAD with a merge still to perform, but only while HEAD is
    /// still at `ours`: never put a value back over one that moved on.
    pub fn set_head_pending(&self, ours: &str, theirs: &str) -> Result<()> {
        if self.head()?.as_deref() != Some(ours) {
            return Ok(());
        }
        let tmp = self.dir.join(format!("HEAD.tmp{}", std::process::id()));
        fs::write(&tmp, format!("{}\nmerge {}\n", ours, theirs))?;
        fs::rename(tmp, self.dir.join("HEAD"))?;
        Ok(())
    }

    /// Rewrite `.tasks/log` from the whole history, oldest first, with the
    /// plan's current numbering. Used after a merge, and whenever the log
    /// is found out of step with HEAD.
    pub fn rebuild_log(&self, history: &[(Hash, Event)], plan: &crate::plan::Plan) -> Result<()> {
        let mut s = String::from(LOG_HEADER);
        for (h, e) in history.iter().rev() {
            s.push_str(&crate::commit::log_line(h, e, plan));
            s.push('\n');
        }
        let tmp = self.dir.join(format!("log.tmp{}", std::process::id()));
        fs::write(&tmp, s)?;
        fs::rename(tmp, self.dir.join("log"))?;
        Ok(())
    }

    /// The event named by the last line of the log, if any.
    pub fn last_logged(&self) -> Option<String> {
        let text = fs::read_to_string(self.dir.join("log")).ok()?;
        let line = text.lines().rev().find(|l| !l.starts_with('#') && !l.trim().is_empty())?;
        line.split_whitespace().nth(1).map(|s| s.to_string())
    }

    pub fn set_head(&self, hash: &str) -> Result<()> {
        let tmp = self.dir.join(format!("HEAD.tmp{}", std::process::id()));
        fs::write(&tmp, format!("{}\n", hash))?;
        fs::rename(tmp, self.dir.join("HEAD"))?;
        Ok(())
    }

    /// Claim the right to advance HEAD from `from`: an atomic create of
    /// `.tasks/claims/<from>`. Only one process ever succeeds for a given
    /// parent, so no event can be committed on top of a parent that was
    /// already committed on top of, whatever happened to the lock. A claim
    /// that already exists means the store moved under this command.
    pub fn claim(&self, from: &str) -> Result<()> {
        let dir = self.dir.join("claims");
        fs::create_dir_all(&dir)?;
        self.ensure_ignored("claims/");
        match fs::OpenOptions::new().write(true).create_new(true).open(dir.join(from)) {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(StoreMoved(from.to_string()).into()),
            Err(e) => Err(e).with_context(|| format!("claiming {}", from)),
        }
    }

    /// Advance HEAD from `from`, the event the caller built on, to `to`,
    /// claiming `from` first; record the move in `.tasks/reflog` so any
    /// earlier state can be found and returned to with `tsk reset`. The
    /// claim is on the caller's parent, never on HEAD as re-read now: if
    /// HEAD has moved on since the caller read it, the claim fails and
    /// nothing is overwritten.
    pub fn advance_head(&self, from: Option<&str>, to: &str, what: &str) -> Result<()> {
        if let Some(f) = from {
            self.claim(f)?;
        }
        self.set_head(to)?;
        self.reflog_append(from, to, what)
    }

    /// Move HEAD from wherever it is now (read under the lock) to `hash`.
    /// For deliberate moves such as `reset`, not for commits, which know
    /// their parent.
    pub fn move_head(&self, hash: &str, what: &str) -> Result<()> {
        let old = self.head()?;
        self.advance_head(old.as_deref(), hash, what)
    }

    /// One line per HEAD move, oldest first: `old new time\tauthor\twhat`.
    /// Local to this copy, like git's reflog; never committed.
    pub fn reflog_append(&self, old: Option<&str>, new: &str, what: &str) -> Result<()> {
        let mut f = fs::OpenOptions::new().append(true).create(true).open(self.dir.join("reflog"))?;
        writeln!(f, "{} {} {}\t{}\t{}", old.unwrap_or("-"), new, crate::commit::now(), self.author(), what.replace(['\n', '\r', '\t'], " "))?;
        Ok(())
    }

    /// The reflog, newest first.
    pub fn reflog(&self) -> Result<Vec<ReflogEntry>> {
        let text = fs::read_to_string(self.dir.join("reflog")).unwrap_or_default();
        let mut out = Vec::new();
        for line in text.lines() {
            let mut parts = line.splitn(3, '\t');
            let head = parts.next().unwrap_or("");
            let author = parts.next().unwrap_or("").to_string();
            let what = parts.next().unwrap_or("").to_string();
            let mut h = head.split_whitespace();
            let (Some(old), Some(new), Some(time)) = (h.next(), h.next(), h.next()) else { continue };
            out.push(ReflogEntry { old: if old == "-" { None } else { Some(old.to_string()) }, new: new.to_string(), time: time.to_string(), author, what });
        }
        out.reverse();
        Ok(out)
    }

    pub fn head_event(&self) -> Result<Option<(Hash, Event)>> {
        match self.head()? {
            None => Ok(None),
            Some(h) => Ok(Some((h.clone(), self.read_object(ObjectType::Event, &h)?))),
        }
    }

    pub fn head_snapshot(&self) -> Result<Snapshot> {
        match self.head_event()? {
            None => Ok(Snapshot::default()),
            Some((_, ev)) => self.read_snapshot(&ev.snapshot),
        }
    }

    /// Every event from HEAD back to the first, newest first.
    pub fn history(&self) -> Result<Vec<(Hash, Event)>> {
        self.history_from(self.head()?)
    }

    /// Every event from `start` back to the first, newest first.
    pub fn history_from(&self, start: Option<Hash>) -> Result<Vec<(Hash, Event)>> {
        let mut out = Vec::new();
        let mut cur = start;
        while let Some(h) = cur {
            let ev: Event = self.read_object(ObjectType::Event, &h)?;
            cur = ev.parent.clone();
            out.push((h, ev));
        }
        Ok(out)
    }

    // ----- lock ----------------------------------------------------------

    /// Take the writer lock. Waits (with backoff) up to `lock.timeout`
    /// seconds for another tsk to finish; breaks a lock older than
    /// `lock.stale` seconds, which only a crashed tsk leaves behind. Both
    /// come from config or `TSK_LOCK_TIMEOUT` / `TSK_LOCK_STALE`.
    pub fn lock(&self) -> Result<Lock> {
        let cfg = self.read_config().unwrap_or_default();
        let secs = |key: &str, env: &str, default: u64| -> u64 {
            std::env::var(env)
                .ok()
                .or_else(|| cfg.get(key).cloned())
                .and_then(|s| s.parse().ok())
                .unwrap_or(default)
        };
        let timeout = Duration::from_secs(secs("lock.timeout", "TSK_LOCK_TIMEOUT", 30));
        let stale = Duration::from_secs(secs("lock.stale", "TSK_LOCK_STALE", 60));
        let path = self.dir.join("lock");
        let start = Instant::now();
        let mut delay = Duration::from_millis(5);
        // Asking the OS whether a holder is alive spawns a process; ask once
        // per distinct holder, not on every turn of the wait.
        let mut checked: Option<(String, bool)> = None;
        loop {
            match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    let args: Vec<String> = std::env::args().skip(1).collect();
                    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
                    let token = format!("{:x}-{:x}", nanos, std::process::id());
                    let _ = writeln!(f, "pid {}\ntoken {}\ntime {}\ncmd {}", std::process::id(), token, crate::commit::now(), args.join(" "));
                    return Ok(Lock { path, token });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // A file time a hair in the future (clock granularity) counts as age zero.
                    let age = fs::metadata(&path).and_then(|m| m.modified()).ok().map(|m| m.elapsed().unwrap_or(Duration::ZERO));
                    let holder = fs::read_to_string(&path).unwrap_or_default();
                    // A lock whose process has exited (a crash) is broken at
                    // once when that can be told; otherwise after `stale`.
                    let dead = age.map(|a| a >= Duration::from_millis(200)).unwrap_or(false) && {
                        match &checked {
                            Some((h, d)) if *h == holder => *d,
                            _ => {
                                let d = lock_holder_exited(&holder);
                                checked = Some((holder.clone(), d));
                                d
                            }
                        }
                    };
                    if let Some(age) = age {
                        if dead || age >= stale {
                            // Take the file aside, then make sure it is the one
                            // we judged: a newer lock may have replaced it since
                            // we looked. If so, give it back.
                            let aside = self.dir.join(format!("lock.breaking-{}", std::process::id()));
                            if fs::rename(&path, &aside).is_ok() {
                                let taken = fs::read_to_string(&aside).unwrap_or_default();
                                let same = lock_token(&taken) == lock_token(&holder) && !holder.is_empty();
                                if same {
                                    let _ = fs::remove_file(&aside);
                                    let why = if dead { "its process has exited".to_string() } else { format!("{}s old", age.as_secs()) };
                                    eprintln!("tsk: broke a stale lock ({}; {})", why, holder.replace('\n', "; ").trim_end_matches("; "));
                                } else if fs::rename(&aside, &path).is_err() {
                                    // A third process took the lock meanwhile; the
                                    // one we displaced will see its token gone.
                                    let _ = fs::remove_file(&aside);
                                }
                                continue;
                            }
                        }
                    }
                    if start.elapsed() > timeout {
                        let holder = fs::read_to_string(&path).unwrap_or_default();
                        bail!(
                            "the store is locked by another tsk ({}); waited {}s.\nIf no tsk is running, remove {}",
                            holder.replace('\n', "; ").trim_end_matches("; "),
                            timeout.as_secs(),
                            path.display()
                        );
                    }
                    let jitter = (start.elapsed().subsec_nanos() % 7) as u64;
                    std::thread::sleep(delay + Duration::from_millis(jitter));
                    delay = (delay * 2).min(Duration::from_millis(100));
                }
                Err(e) => return Err(e).with_context(|| format!("taking {}", path.display())),
            }
        }
    }

    // ----- log -----------------------------------------------------------

    /// Append one line to `.tasks/log`. Called under the lock.
    pub fn append_log(&self, line: &str) -> Result<()> {
        let p = self.dir.join("log");
        let mut f = fs::OpenOptions::new().append(true).create(true).open(&p)?;
        if f.metadata()?.len() == 0 {
            f.write_all(LOG_HEADER.as_bytes())?;
        }
        writeln!(f, "{}", line)?;
        Ok(())
    }

    // ----- config --------------------------------------------------------

    pub fn read_config(&self) -> Result<BTreeMap<String, String>> {
        let p = self.dir.join("config");
        let mut map = BTreeMap::new();
        if p.exists() {
            for line in fs::read_to_string(p)?.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((k, v)) = line.split_once('=') {
                    map.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
        }
        Ok(map)
    }

    pub fn write_config(&self, map: &BTreeMap<String, String>) -> Result<()> {
        let mut s = String::from("# tsk config: key = value\n");
        for (k, v) in map {
            s.push_str(&format!("{} = {}\n", k, v));
        }
        fs::write(self.dir.join("config"), s)?;
        Ok(())
    }

    pub fn config_value(&self, key: &str) -> Option<String> {
        self.read_config().ok().and_then(|c| c.get(key).cloned())
    }

    /// The format the store declares; none (before 0.5.0) is 0.
    pub fn format(&self) -> u32 {
        self.config_value("format").and_then(|v| v.trim().parse().ok()).unwrap_or(0)
    }

    /// Refuse a store written by a newer tsk rather than misread it.
    pub fn check_format(&self) -> Result<()> {
        let f = self.format();
        if f > FORMAT {
            bail!("this store is format {} (written by a newer tsk); tsk {} reads up to format {}: upgrade tsk", f, env!("CARGO_PKG_VERSION"), FORMAT);
        }
        Ok(())
    }

    /// Declare this binary's format, on the first write by a newer binary.
    pub fn mark_format(&self) -> Result<()> {
        if self.format() < FORMAT {
            let mut cfg = self.read_config()?;
            cfg.insert("format".to_string(), FORMAT.to_string());
            self.write_config(&cfg)?;
        }
        Ok(())
    }

    /// Who signs events: `TSK_AUTHOR` first, so that each agent session on a
    /// shared store signs its own; then the store's `author`; then the OS user.
    pub fn author(&self) -> String {
        if let Ok(a) = std::env::var("TSK_AUTHOR") {
            if !a.trim().is_empty() {
                return a;
            }
        }
        if let Some(a) = self.config_value("author") {
            return a;
        }
        std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "unknown".to_string())
    }
}
