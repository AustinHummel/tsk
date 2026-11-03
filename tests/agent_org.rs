//! 0.2: what a hierarchical agent org needs. Parallel writers, a store
//! named from anywhere, fields and docs, the chain of intent, the markdown
//! mirror, the log file and plan diff, pruning, and the old snapshot form.

mod common;
use common::{dana, Proj};
use std::process::Command;

#[test]
fn parallel_writers_never_lose_an_event() {
    let p = Proj::new("parallel");
    let elsewhere = std::env::temp_dir();
    let store = p.store();
    let mut kids = Vec::new();
    for i in 0..16 {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tsk"));
        cmd.args(["--store", &store, "add", &format!("task {}", i)]).current_dir(&elsewhere).env("TSK_AUTHOR", format!("agent-{}", i));
        kids.push(cmd.spawn().unwrap());
    }
    for k in &mut kids {
        assert!(k.wait().unwrap().success());
    }
    assert_eq!(p.ok(&["rev-list"]).lines().count(), 17, "init + 16 adds");
    assert_eq!(p.ok(&["plan"]).lines().count(), 16);
    let log = p.read(".tasks/log");
    assert_eq!(log.lines().filter(|l| !l.starts_with('#')).count(), 17, "{}", log);
    assert!(log.contains("-- agent-7"), "events carry each agent's identity:\n{}", log);
    assert!(!p.dir.join(".tasks/lock").exists(), "the lock is released");
    // A lock held by a live process is waited on, then reported.
    std::fs::write(p.dir.join(".tasks/lock"), format!("pid {}\n", std::process::id())).unwrap();
    let (ok, text) = p.run_from(&p.dir, &[("TSK_LOCK_TIMEOUT", "1")], &["add", "blocked"]);
    assert!(!ok && text.contains("locked by another tsk"), "{}", text);
    // One whose process has exited (a crash) is broken at once; one too old
    // to tell is broken after `lock.stale`.
    // A process that has just exited: dead by construction on every OS.
    let mut child = Command::new(env!("CARGO_BIN_EXE_tsk")).arg("--version").stdout(std::process::Stdio::null()).spawn().unwrap();
    let dead_pid = child.id();
    child.wait().unwrap();
    std::fs::write(p.dir.join(".tasks/lock"), format!("pid {}\n", dead_pid)).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(250));
    let (ok, text) = p.run_from(&p.dir, &[], &["add", "after crash"]);
    assert!(ok && text.contains("its process has exited"), "{}", text);
    std::fs::write(p.dir.join(".tasks/lock"), "no pid here\n").unwrap();
    let (ok, text) = p.run_from(&p.dir, &[("TSK_LOCK_STALE", "0")], &["add", "after stale"]);
    assert!(ok && text.contains("broke a stale lock"), "{}", text);
}

#[test]
fn store_is_addressable_without_changing_directory() {
    let p = Proj::new("addr");
    p.ok(&["add", "root"]);
    let elsewhere = std::env::temp_dir();
    let (ok, text) = p.run_from(&elsewhere, &[], &["--store", &p.store(), "plan"]);
    assert!(ok && text.contains("root"), "{}", text);
    let project = p.dir.to_string_lossy().to_string();
    let (ok, text) = p.run_from(&elsewhere, &[("TSK_STORE", &project)], &["show", "1"]);
    assert!(ok && text.contains("root"), "{}", text);
    let (ok, text) = p.run_from(&elsewhere, &[], &["-C", &project, "ready"]);
    assert!(ok && text.contains("root"), "{}", text);
    let (ok, text) = p.run_from(&elsewhere, &[], &["--store", "C:/nowhere/at/all", "plan"]);
    assert!(!ok && text.contains("no store at"), "{}", text);
}

#[test]
fn fields_and_docs_are_recorded_shown_and_filterable() {
    let p = Proj::new("fields");
    dana(&p);
    p.ok(&["set", "1.2", "agent=local_abc", "run=runs/r1"]);
    assert!(p.ok(&["set", "1.2", "agent=local_abc"]).contains("nothing changed"));
    assert!(p.fails(&["set", "1.2", "bad key=1"]).contains("field key"));
    assert!(p.fails(&["set", "1.2", "novalue"]).contains("key=value"));
    let show = p.ok(&["show", "1.2"]);
    assert!(show.contains("fields: agent=local_abc run=runs/r1"), "{}", show);
    assert!(p.ok(&["--json", "show", "1.2"]).contains("\"agent\": \"local_abc\""));
    // Inherited: 1.2's children match the filter; 1.1 does not.
    let f = p.file("bd.md", "= child: store\n= child: pick\n= narrative\nx\n= req r2 -> 2\n= req r3 -> 2\n= req r4 -> all\n");
    p.ok(&["breakdown", "1.2", "-F", &f]);
    let plan = p.ok(&["plan", "--where", "agent=local_abc"]);
    assert!(plan.contains("1.2.1") && !plan.contains("1.1 "), "{}", plan);
    let ready = p.ok(&["ready", "--where", "agent"]);
    assert!(ready.contains("1.2.1") && !ready.contains("1.1 "), "{}", ready);
    assert!(p.ok(&["ready", "--where", "agent=other"]).contains("nothing is ready"));
    p.ok(&["set", "1.2", "--unset", "run"]);
    assert!(!p.ok(&["show", "1.2"]).contains("run="));
    // Fields on a requirement.
    p.ok(&["set", "--req", "r2", "kind=interface"]);
    assert!(p.ok(&["show", "1.1"]).contains("{kind=interface}"));
    assert!(p.fails(&["set", "--req", "r99", "a=b"]).contains("no requirement"));
    // Docs.
    p.ok(&["doc", "1.2", "org/cli/README.md"]);
    assert!(p.ok(&["doc", "1.2"]).contains("org/cli/README.md"));
    assert!(p.ok(&["show", "1.2"]).contains("docs: org/cli/README.md"));
    p.ok(&["doc", "1.2", "--rm", "org/cli/README.md"]);
    assert!(p.ok(&["doc", "1.2"]).contains("no docs"));
    let log = p.ok(&["log"]);
    assert!(log.contains("set ") && log.contains("doc 1.2 +org/cli/README.md") && log.contains("doc 1.2 -org/cli/README.md"), "{}", log);
}

#[test]
fn context_prints_the_chain_of_intent() {
    let p = Proj::new("context");
    dana(&p);
    p.ok(&["doc", "1", "notes/dana.md"]);
    p.ok(&["set", "1.2", "agent=a1"]);
    let f = p.file("bd.md", "= child: store\n= child: pick\n= narrative\nstore then pick\n= req r2 -> 2\n= req r3 -> 2\n= req r4 -> all\n");
    p.ok(&["breakdown", "1.2", "-F", &f]);
    let ctx = p.ok(&["context", "1.2.2"]);
    for needle in [
        "chain of intent",
        "required by Dana: the same two picks always win",
        "doc: notes/dana.md",
        "fields: agent=a1",
        "understand, build, hand over",
        "store then pick",
        "<- this task",
        "r2 predecessor [1.1 t2 Gather requirements] (todo): what fair means",
        "r4 via 1.2: successor [1.3 t4 Show Dana]: build before showing",
        "siblings:",
        "1.2.1 store",
    ] {
        assert!(ctx.contains(needle), "missing {:?} in:\n{}", needle, ctx);
    }
    let js: serde_json::Value = serde_json::from_str(&p.ok(&["--json", "context", "1.2.2"])).unwrap();
    assert_eq!(js["chain"].as_array().unwrap().len(), 3);
    assert_eq!(js["chain"][0]["docs"][0], "notes/dana.md");
    assert_eq!(js["inherited"].as_array().unwrap().len(), 0);
    assert_eq!(js["inherited_required_by"][0]["req"]["id"], "r4");
}

#[test]
fn render_is_idempotent_and_backlinks_find_notes() {
    let p = Proj::new("render");
    dana(&p);
    p.ok(&["set", "1.2", "agent=a1"]);
    p.ok(&["doc", "1.2", "notes/cli.md"]);
    let out = p.ok(&["render"]);
    assert!(out.contains("4 page(s)") && out.contains("4 changed"), "{}", out);
    assert!(p.ok(&["render"]).contains("0 changed"));
    let page = p.read("tasks/t3.md");
    assert!(page.contains("aliases:\n  - \"1.2 Build a CLI\""), "{}", page);
    assert!(page.contains("agent: \"a1\""), "{}", page);
    assert!(page.contains("[notes/cli.md](../notes/cli.md)"), "{}", page);
    assert!(page.contains("[1.1 Gather requirements](t2.md)"), "{}", page);
    let index = p.read("tasks/README.md");
    assert!(index.contains("- [ ] [1 Help Dana](t1.md)"), "{}", index);
    assert!(index.contains("  - [ ] [1.2 Build a CLI](t3.md)"), "{}", index);
    // A note links to the page both ways Obsidian writes links.
    p.file("notes/cli.md", "# CLI\nShaped by [[t3]] and [the task](../tasks/t3.md).\n");
    let bl = p.ok(&["backlinks", "1.2"]);
    assert!(bl.contains("notes/cli.md:2"), "{}", bl);
    assert!(!bl.lines().any(|l| l.starts_with("tasks/")), "rendered pages are not backlinks: {}", bl);
    assert!(p.ok(&["backlinks", "1.1"]).contains("nothing links"));
    // A custom folder, from the flag and from config.
    assert!(p.ok(&["render", "--dir", "mirror"]).contains("mirror"));
    assert!(p.dir.join("mirror/t1.md").exists());
    p.ok(&["config", "render.dir", "plan/pages"]);
    assert!(p.ok(&["render"]).contains("plan"));
    assert!(p.dir.join("plan/pages/t1.md").exists());
}

#[test]
fn log_file_and_plan_diff_track_every_event() {
    let p = Proj::new("logdiff");
    dana(&p);
    let log = p.read(".tasks/log");
    let lines: Vec<&str> = log.lines().filter(|l| !l.starts_with('#')).collect();
    assert_eq!(lines.len(), p.ok(&["rev-list"]).lines().count());
    assert!(lines[1].contains("created") && lines[1].contains("add Help Dana") && lines[1].contains("[1]"), "{}", lines[1]);
    assert!(p.read(".tasks/.gitignore").contains("lock"));
    // Whole-plan diff from the first event to HEAD.
    let first = p.ok(&["rev-list"]).lines().last().unwrap().to_string();
    let d = p.ok(&["diff", "--from", &first]);
    assert!(d.contains("+ 1 t1 Help Dana") && d.contains("+ 1.2 t3 Build a CLI") && d.contains("+ r2 blocking"), "{}", d);
    assert!(d.contains("5 event(s)"), "{}", d);
    // With no git, the default basis is the previous event.
    p.ok(&["complete", "1.1"]);
    let d = p.ok(&["diff"]);
    assert!(d.contains("since the previous event") && d.contains("todo -> complete"), "{}", d);
    let js: serde_json::Value = serde_json::from_str(&p.ok(&["--json", "diff", "--from", &first])).unwrap();
    assert!(js["changes"].as_array().unwrap().len() >= 7, "{}", js);
}

#[test]
fn diff_against_the_last_git_commit() {
    let p = Proj::new("gitdiff");
    let git = |args: &[&str]| {
        let out = Command::new("git").args(args).current_dir(&p.dir).output().expect("git on PATH");
        assert!(out.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
    };
    git(&["init", "-q"]);
    dana(&p);
    git(&["add", "."]);
    git(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "plan"]);
    assert!(p.ok(&["diff"]).contains("no changes since"), "nothing since the commit");
    p.ok(&["complete", "1.1"]);
    p.ok(&["set", "1.2", "agent=a1"]);
    let d = p.ok(&["diff"]);
    assert!(d.contains("2 event(s)") && d.contains("since git HEAD") && d.contains("(fields)"), "{}", d);
    assert!(p.ok(&["diff", "--git", "HEAD"]).contains("2 event(s)"));
    assert!(!p.dir.join(".tasks/lock").exists());
}

#[test]
fn prune_keeps_every_event_and_the_current_plan() {
    let p = Proj::new("prune");
    dana(&p);
    let revs: Vec<String> = p.ok(&["rev-list"]).lines().map(String::from).collect();
    let before = revs.len();
    let cutoff = &revs[1];
    let dry = p.ok(&["prune", "--dry-run", "--before", cutoff]);
    assert!(dry.contains("would go") && dry.contains("event(s) shallow"), "{}", dry);
    assert_eq!(p.ok(&["rev-list"]).lines().count(), before, "a dry run writes nothing");
    let out = p.ok(&["prune", "--before", cutoff]);
    assert!(out.contains("removed"), "{}", out);
    assert_eq!(p.ok(&["rev-list"]).lines().count(), before + 1, "every event stays, plus the prune event");
    assert!(p.ok(&["plan"]).contains("1.2 Build a CLI"));
    assert!(p.ok(&["log"]).contains("init"), "the log still reaches the first event");
    let oldest = revs.last().unwrap();
    assert!(p.fails(&["ls-snapshot", oldest]).contains("pruned"));
    assert!(p.fails(&["diff", "--from", oldest]).contains("pruned"));
    assert!(p.ok(&["ls-snapshot", cutoff]).contains("task"), "the cutoff event keeps its snapshot");
    assert!(p.ok(&["prune"]).contains("nothing to prune"));
}

#[test]
fn flat_snapshots_from_0_1_still_load() {
    use sha2::Digest;
    let p = Proj::new("compat");
    p.ok(&["add", "old root"]);
    // Hand-write a 0.1-style event: a flat snapshot naming the task object.
    let objects = p.dir.join(".tasks/objects");
    let write = |ty: &str, json: &str| -> String {
        let text = format!("{}\n{}\n", ty, json);
        let hash = hex::encode(sha2::Sha256::digest(text.as_bytes()));
        let dir = objects.join(&hash[..2]);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(&hash[2..]), text).unwrap();
        hash
    };
    let task = write("task", r#"{"id":"t1","title":"old root (flat)","parent":null,"seq":1,"body":"","types":[]}"#);
    let snap = write("snapshot", &format!(r#"{{"tasks":{{"t1":"{}"}},"reqs":{{}},"types":{{}},"next_task":1,"next_req":0}}"#, task));
    let head = p.ok(&["rev-list"]).lines().next().unwrap().to_string();
    let ev = write(
        "event",
        &format!(r#"{{"kind":"Edited","parent":"{}","snapshot":"{}","time":"2026-10-30T00:00:00Z","author":"old","message":"edit 1 (title)","subjects":["t1"]}}"#, head, snap),
    );
    std::fs::write(p.dir.join(".tasks/HEAD"), format!("{}\n", ev)).unwrap();
    assert!(p.ok(&["plan"]).contains("old root (flat)"));
    assert!(p.ok(&["ls-snapshot"]).contains(&task));
    // The next change writes the tree form, and the history spans both.
    p.ok(&["add", "new root"]);
    assert!(p.ok(&["plan"]).contains("2 new root"));
    assert!(p.ok(&["diff", "--from", &head]).contains("+ 2 t2 new root"));
    let newest = p.ok(&["rev-list"]).lines().next().unwrap().to_string();
    let out = p.ok(&["cat-object", &newest]);
    let snap_hash = out.lines().find(|l| l.contains("\"snapshot\"")).unwrap().split('"').nth(3).unwrap().to_string();
    assert!(p.ok(&["cat-object", &snap_hash]).contains("\"version\": 2"));
}

#[test]
fn reasons_and_bodies_may_start_with_a_dash() {
    let p = Proj::new("dash");
    dana(&p);
    p.ok(&["link", "1.1", "1.3", "--git is the flag it needs"]);
    p.ok(&["amend", "1", "Fixes", "--narrative", "- found while dogfooding", "-b", "- a bullet list"]);
    assert!(p.ok(&["show", "1.4"]).contains("- a bullet list"));
    p.ok(&["drop", "1.4", "-- not needed"]);
}

#[test]
fn a_type_can_stand_for_a_process_with_docs_and_conditions() {
    let p = Proj::new("process-type");
    p.ok(&["add", "root"]);
    let f = p.file("bd.md", "= child: ship the fix\n= child: ship the docs\n= narrative\nx\n");
    p.ok(&["breakdown", "1", "-F", &f]);
    p.ok(&["type", "add", "change", "A change to the product", "--doc", "skills/change/SKILL.md"]);
    p.ok(&["type", "item", "change", "tested", "The change is covered by a test that fails without it"]);
    p.ok(&["type", "item", "change", "noted", "The user-facing note is written", "--when", "the change is visible to users", "--doc", "docs/notes.md"]);
    let show = p.ok(&["type", "show", "change"]);
    assert!(show.contains("docs: skills/change/SKILL.md") && show.contains("when the change is visible to users") && show.contains("docs docs/notes.md"), "{}", show);
    p.ok(&["apply", "change", "1.1"]);
    // Instances carry the type's docs and the item's docs, and the condition.
    let core = p.ok(&["show", "1.1.1"]);
    assert!(core.contains("core") && !core.contains("docs:"), "{}", core);
    let tested = p.ok(&["show", "1.1.2"]);
    assert!(tested.contains("docs: skills/change/SKILL.md"), "{}", tested);
    let noted = p.ok(&["show", "1.1.3"]);
    assert!(noted.contains("docs: skills/change/SKILL.md, docs/notes.md") && noted.contains("When: the change is visible to users"), "{}", noted);
    let ctx = p.ok(&["context", "1.1.3"]);
    assert!(ctx.contains("doc: skills/change/SKILL.md"), "the process is one hop from the instance: {}", ctx);
    // An item that does not apply is dropped with the reason: a recorded exception, not a violation.
    p.ok(&["drop", "1.1.3", "nothing user-visible in this fix"]);
    p.ok(&["complete", "1.1.1", "1.1.2"]);
    assert!(p.ok(&["plan", "-a"]).contains("[x] 1.1 ship the fix"));
    assert!(p.ok(&["forced"]).contains("no forced events"));
}

#[test]
fn a_commit_on_a_parent_already_advanced_is_refused_not_overwritten() {
    let p = Proj::new("cas");
    dana(&p);
    let head = p.read(".tasks/HEAD").trim().to_string();
    let log_before = p.read(".tasks/log");
    // Another process has advanced from this HEAD (its claim exists), but
    // this process still believes HEAD is here: the write must not land.
    std::fs::create_dir_all(p.dir.join(".tasks/claims")).unwrap();
    std::fs::write(p.dir.join(".tasks/claims").join(&head), "").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_tsk")).args(["add", "lost?"]).current_dir(&p.dir).output().unwrap();
    assert_eq!(out.status.code(), Some(3), "exit 3 says: run it again");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("the store moved") && err.contains("nothing was written"), "{}", err);
    assert_eq!(p.read(".tasks/HEAD").trim(), head, "HEAD untouched");
    assert_eq!(p.read(".tasks/log"), log_before, "no log line for a write that did not happen");
    assert!(!p.ok(&["plan"]).contains("lost?"));
    assert!(!p.dir.join(".tasks/lock").exists(), "the lock is released on the way out");
    std::fs::remove_file(p.dir.join(".tasks/claims").join(&head)).unwrap();
    p.ok(&["add", "fine now"]);
    assert!(p.read(".tasks/.gitignore").contains("claims/"));
}

#[test]
fn a_reported_save_is_always_on_the_line_even_with_stale_locks_broken_at_once() {
    // The trial's scenario: lock.stale = 0, many writers at once. Every
    // process that reports success must have its event on the line; the
    // rest must say so and exit 3.
    let p = Proj::new("stale0");
    let store = p.store();
    let before = p.ok(&["rev-list"]).lines().count();
    let mut kids = Vec::new();
    for i in 0..20 {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tsk"));
        cmd.args(["--store", &store, "add", &format!("task {}", i)]).env("TSK_LOCK_STALE", "0").env("TSK_LOCK_TIMEOUT", "20");
        kids.push(cmd.spawn().unwrap());
    }
    let mut ok = 0;
    let mut moved = 0;
    for k in &mut kids {
        match k.wait().unwrap().code() {
            Some(0) => ok += 1,
            Some(3) => moved += 1,
            other => panic!("unexpected exit {:?}", other),
        }
    }
    let after = p.ok(&["rev-list"]).lines().count();
    assert_eq!(after - before, ok, "{} reported saved, {} on the line, {} told to retry", ok, after - before, moved);
    assert_eq!(p.ok(&["plan"]).lines().count(), ok);
    assert!(ok >= 1);
    // The log agrees with the line.
    let log_lines = p.read(".tasks/log").lines().filter(|l| !l.starts_with('#')).count();
    assert_eq!(log_lines, after);
}
