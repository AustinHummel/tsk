//! Shared helpers: a temp project with a store, run by the built binary.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Proj {
    pub dir: PathBuf,
}

impl Proj {
    pub fn new(name: &str) -> Proj {
        let dir = std::env::temp_dir().join(format!("tsk-test-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = Proj { dir };
        p.ok(&["init"]);
        p
    }
    pub fn run(&self, args: &[&str]) -> (bool, String) {
        self.run_from(&self.dir, &[], args)
    }
    /// Run from any directory with extra environment, e.g. naming the store.
    pub fn run_from(&self, cwd: &Path, env: &[(&str, &str)], args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tsk"));
        cmd.args(args).current_dir(cwd);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        (out.status.success(), text)
    }
    pub fn ok(&self, args: &[&str]) -> String {
        let (ok, text) = self.run(args);
        assert!(ok, "expected success for {:?}:\n{}", args, text);
        text
    }
    pub fn fails(&self, args: &[&str]) -> String {
        let (ok, text) = self.run(args);
        assert!(!ok, "expected failure for {:?}:\n{}", args, text);
        text
    }
    pub fn file(&self, name: &str, content: &str) -> String {
        let p = self.dir.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, content).unwrap();
        p.to_string_lossy().to_string()
    }
    pub fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.dir.join(rel)).unwrap_or_default()
    }
    pub fn store(&self) -> String {
        self.dir.join(".tasks").to_string_lossy().to_string()
    }
}

impl Drop for Proj {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Dana's plan: one root with three children and three links.
pub fn dana(p: &Proj) {
    p.ok(&["add", "Help Dana", "--by", "Dana: the same two picks always win"]);
    let f = p.file(
        "bd1.md",
        "= child: Gather requirements\n= child: Build a CLI\n= child: Show Dana\n= narrative\nunderstand, build, hand over\n",
    );
    p.ok(&["breakdown", "1", "-F", &f]);
    p.ok(&["link", "1.1", "1.2", "what fair means"]);
    p.ok(&["link", "1.1", "1.2", "do we need help"]);
    p.ok(&["link", "1.2", "1.3", "build before showing"]);
}
