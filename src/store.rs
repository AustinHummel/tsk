//! The object store: `.tasks/` at the project root, found by walking up.
//! Objects are plain UTF-8 text files named by the SHA-256 of their content,
//! `<type>\n<json>`. `HEAD` holds the hash of the latest event.

use crate::model::*;
use anyhow::{anyhow, bail, Context, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub const DIR: &str = ".tasks";

pub struct Store {
    pub dir: PathBuf,
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
        bail!("not a tsk project (no {} found here or above); run `tsk init`", DIR)
    }

    // ----- objects -------------------------------------------------------

    pub fn hash_bytes(bytes: &[u8]) -> Hash {
        hex::encode(Sha256::digest(bytes))
    }

    fn object_path(&self, hash: &str) -> PathBuf {
        self.dir.join("objects").join(&hash[..2]).join(&hash[2..])
    }

    pub fn write_object<T: Serialize>(&self, ty: ObjectType, value: &T) -> Result<Hash> {
        let json = serde_json::to_string_pretty(value)?;
        let text = format!("{}\n{}\n", ty.name(), json);
        let hash = Self::hash_bytes(text.as_bytes());
        let path = self.object_path(&hash);
        if !path.exists() {
            fs::create_dir_all(path.parent().unwrap())?;
            fs::write(&path, text)?;
        }
        Ok(hash)
    }

    pub fn read_raw(&self, hash: &str) -> Result<(ObjectType, String)> {
        let full = self.resolve_hash(hash)?;
        let text = fs::read_to_string(self.object_path(&full))
            .with_context(|| format!("object {} not found", hash))?;
        let (ty, body) = text
            .split_once('\n')
            .ok_or_else(|| anyhow!("object {} is malformed", hash))?;
        let ty = ObjectType::parse(ty).ok_or_else(|| anyhow!("object {} has unknown type {}", hash, ty))?;
        Ok((ty, body.to_string()))
    }

    pub fn read_object<T: DeserializeOwned>(&self, ty: ObjectType, hash: &str) -> Result<T> {
        let (actual, body) = self.read_raw(hash)?;
        if actual != ty {
            bail!("object {} is a {}, not a {}", hash, actual.name(), ty.name());
        }
        Ok(serde_json::from_str(&body)?)
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
                if name.starts_with(&prefix[2..]) {
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

    // ----- HEAD ----------------------------------------------------------

    pub fn head(&self) -> Result<Option<Hash>> {
        let p = self.dir.join("HEAD");
        if !p.exists() {
            return Ok(None);
        }
        let s = fs::read_to_string(p)?.trim().to_string();
        Ok(if s.is_empty() { None } else { Some(s) })
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
            Some((_, ev)) => self.read_object(ObjectType::Snapshot, &ev.snapshot),
        }
    }

    /// Every event from HEAD back to the first, newest first.
    pub fn history(&self) -> Result<Vec<(Hash, Event)>> {
        let mut out = Vec::new();
        let mut cur = self.head()?;
        while let Some(h) = cur {
            let ev: Event = self.read_object(ObjectType::Event, &h)?;
            cur = ev.parent.clone();
            out.push((h, ev));
        }
        Ok(out)
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

    pub fn author(&self) -> String {
        if let Ok(cfg) = self.read_config() {
            if let Some(a) = cfg.get("author") {
                return a.clone();
            }
        }
        std::env::var("TSK_AUTHOR")
            .or_else(|_| std::env::var("USERNAME"))
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "unknown".to_string())
    }
}
