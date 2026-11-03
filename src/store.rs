//! The object store: `.tasks/` at the project root, found by walking up or
//! named outright. Objects are plain UTF-8 text files named by the SHA-256
//! of their content, `<type>\n<json>`. `HEAD` holds the hash of the latest
//! event. `lock` serializes writers. `log` is the human-readable event log.

use crate::model::*;
use anyhow::{anyhow, bail, Context, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const DIR: &str = ".tasks";

/// What is ignored inside the store when it is committed with git.
const GITIGNORE: &str = "# transient files of a running tsk\nlock\nlock.broken-*\nHEAD.tmp\n*.EDIT\n*.tmp\n";

/// Objects are named by the hash of their bytes: git must never convert
/// their line endings. The log stays diffable.
pub const GITATTRIBUTES: &str = "* -text\nlog text eol=lf merge=union\nHEAD merge=tsk\n";

const LOG_HEADER: &str = "# tsk event log: one line per event, oldest first. time  event  kind  message  [tasks]  -- author\n";

pub struct Store {
    pub dir: PathBuf,
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

/// Holds `.tasks/lock` until dropped.
pub struct Lock {
    path: PathBuf,
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl Store {
    pub fn init(at: &Path) -> Result<Store> {
        let dir = at.join(DIR);
        if dir.exists() {
            bail!("{} already exists", dir.display());
        }
        fs::create_dir_all(dir.join("objects"))?;
        let store = Store { dir };
        store.write_config(&BTreeMap::new())?;
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

    /// Write HEAD with a merge still to perform.
    pub fn set_head_pending(&self, ours: &str, theirs: &str) -> Result<()> {
        let tmp = self.dir.join("HEAD.tmp");
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
        let tmp = self.dir.join("log.tmp");
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
        let tmp = self.dir.join("HEAD.tmp");
        fs::write(&tmp, format!("{}\n", hash))?;
        fs::rename(tmp, self.dir.join("HEAD"))?;
        Ok(())
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
        loop {
            match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    let args: Vec<String> = std::env::args().skip(1).collect();
                    let _ = writeln!(f, "pid {}\ntime {}\ncmd {}", std::process::id(), crate::commit::now(), args.join(" "));
                    return Ok(Lock { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let age = fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|m| m.elapsed().ok());
                    if let Some(age) = age {
                        if age > stale {
                            let holder = fs::read_to_string(&path).unwrap_or_default();
                            let broken = self.dir.join(format!("lock.broken-{}", std::process::id()));
                            if fs::rename(&path, &broken).is_ok() {
                                let _ = fs::remove_file(&broken);
                                eprintln!("tsk: broke a stale lock ({}s old; {})", age.as_secs(), holder.replace('\n', "; ").trim_end_matches("; "));
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
