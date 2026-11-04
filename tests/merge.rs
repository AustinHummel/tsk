//! 0.3: merging divergent copies of a store, by hand and through git.

mod common;
use common::{dana, Proj};
use std::path::Path;
use std::process::Command;

/// A second copy of the store, as git would carry it.
fn copy_store(from: &Path, to: &Path) {
    fn walk(a: &Path, b: &Path) {
        std::fs::create_dir_all(b).unwrap();
        for e in std::fs::read_dir(a).unwrap().flatten() {
            let (pa, pb) = (e.path(), b.join(e.file_name()));
            if pa.is_dir() {
                walk(&pa, &pb);
            } else {
                std::fs::copy(&pa, &pb).unwrap();
            }
        }
    }
    walk(&from.join(".tasks"), &to.join(".tasks"));
}

/// Bring the other copy's objects over (add-only, like a git merge).
fn bring_objects(from: &Path, to: &Path) {
    fn walk(a: &Path, b: &Path) {
        std::fs::create_dir_all(b).unwrap();
        for e in std::fs::read_dir(a).unwrap().flatten() {
            let (pa, pb) = (e.path(), b.join(e.file_name()));
            if pa.is_dir() {
                walk(&pa, &pb);
            } else if !pb.exists() {
                std::fs::copy(&pa, &pb).unwrap();
            }
        }
    }
    walk(&from.join(".tasks/objects"), &to.join(".tasks/objects"));
}

fn head(p: &Proj) -> String {
    p.read(".tasks/HEAD").lines().next().unwrap().trim().to_string()
}

#[test]
fn disjoint_changes_merge_with_ids_renumbered() {
    let a = Proj::new("merge-a");
    dana(&a);
    let b = Proj::new("merge-b");
    copy_store(&a.dir, &b.dir);
    a.run_from(&a.dir, &[("TSK_AUTHOR", "alice")], &["add", "alice's root"]);
    a.run_from(&a.dir, &[("TSK_AUTHOR", "alice")], &["set", "1.2", "owner=alice"]);
    b.run_from(&b.dir, &[("TSK_AUTHOR", "bob")], &["add", "bob's root"]);
    b.run_from(&b.dir, &[("TSK_AUTHOR", "bob")], &["link", "2", "1.3", "bob first"]);
    b.run_from(&b.dir, &[("TSK_AUTHOR", "bob")], &["complete", "1.1"]);
    bring_objects(&b.dir, &a.dir);
    let theirs = head(&b);
    let dry = a.ok(&["merge", &theirs, "--dry-run"]);
    assert!(dry.contains("renumbered: ") && dry.contains("t5 is now t6") && dry.contains("would replay"), "{}", dry);
    let before = a.ok(&["rev-list"]).lines().count();
    assert_eq!(a.ok(&["rev-list"]).lines().count(), before, "a dry run writes nothing");
    let out = a.ok(&["merge", &theirs]);
    assert!(out.contains("merged") && out.contains("event(s) replayed"), "{}", out);
    let plan = a.ok(&["plan", "-a"]);
    assert!(plan.contains("alice's root") && plan.contains("bob's root") && plan.contains("[x] 1.1"), "{}", plan);
    assert!(a.ok(&["show", "1.2"]).contains("owner=alice"));
    let show = a.ok(&["show", "1.3"]);
    assert!(show.contains("bob first") && show.contains("bob's root]"), "their link follows the renumbering: {}", show);
    let log = a.read(".tasks/log");
    assert!(log.contains("-- bob") && log.contains("-- alice") && log.lines().last().unwrap().contains("merged"), "the log is rebuilt in chain order: {}", log);
    let js: serde_json::Value = serde_json::from_str(&a.ok(&["--json", "log", "-n", "2"])).unwrap();
    assert!(js[1]["event"]["replayed_from"].is_string(), "{}", js);
    assert!(a.ok(&["rev-list"]).lines().count() > before, "one line of history");
    // Merging again is a no-op. Merging the other way converges: the same
    // plan, the same ids, the same snapshot.
    assert!(a.ok(&["merge", &theirs]).contains("already merged"));
    bring_objects(&a.dir, &b.dir);
    let back = b.ok(&["merge", &head(&a)]);
    assert!(back.contains("merged"), "{}", back);
    assert_eq!(b.ok(&["plan", "-a"]), a.ok(&["plan", "-a"]));
    let snap = |p: &Proj| p.ok(&["cat-object", &head(p)]).lines().find(|l| l.contains("\"snapshot\"")).unwrap().to_string();
    assert_eq!(snap(&a), snap(&b), "both copies hold the identical merged snapshot");
    bring_objects(&b.dir, &a.dir);
    assert!(a.ok(&["merge", &head(&b)]).contains("already merged"), "nothing left to exchange");
    assert!(b.ok(&["merge", &head(&a)]).contains("already merged"));
}

#[test]
fn an_adoption_on_one_side_merges_with_adds_on_the_other() {
    let a = Proj::new("adopt-a");
    dana(&a);
    let b = Proj::new("adopt-b");
    copy_store(&a.dir, &b.dir);
    // Ours adopts: t5 takes position 6 from the counter without a task t6 existing.
    a.ok(&["add", "alice's step"]); // t5
    a.ok(&["adopt", "1", "t5", "--narrative", "it belongs to Dana's work"]);
    assert!(a.ok(&["plan"]).contains("1.4 t5 alice's step"));
    // Theirs adds t5 and t6: both collide, one by id and one by position.
    b.ok(&["add", "bob's root"]); // t5
    b.ok(&["add", "bob's other"]); // t6
    bring_objects(&b.dir, &a.dir);
    let out = a.ok(&["merge", &head(&b)]);
    // Ids collide and are renumbered; which side's depends on which events
    // come first in the canonical order (the same from either copy).
    assert!(out.contains(" is now t"), "{}", out);
    let plan = a.ok(&["plan"]);
    // The number is the third word: the glyph `[ ]` is two.
    let root_of = |title: &str| plan.lines().find(|l| l.contains(title)).map(|l| l.split_whitespace().nth(2).unwrap().to_string()).unwrap_or_else(|| panic!("{} in {}", title, plan));
    assert!(root_of("alice's step") == "1.4", "{}", plan);
    let (r1, r2) = (root_of("bob's root"), root_of("bob's other"));
    assert!(r1 != r2 && !r1.contains('.') && !r2.contains('.'), "{}", plan);
    // The root slot alice's step held stays vacant on the merged line too.
    let vacant = (2..=4).map(|n| n.to_string()).find(|n| *n != r1 && *n != r2).unwrap();
    assert!(a.fails(&["show", &vacant]).contains("is now 1.4"), "{}", plan);
    // The other way converges on the same plan and the same snapshot.
    bring_objects(&a.dir, &b.dir);
    b.ok(&["merge", &head(&a)]);
    assert_eq!(a.ok(&["plan", "-a"]), b.ok(&["plan", "-a"]));
    let snap = |p: &Proj| p.ok(&["cat-object", &head(p)]).lines().find(|l| l.contains("\"snapshot\"")).unwrap().to_string();
    assert_eq!(snap(&a), snap(&b));
    // Every position, held or vacated, is unique on the merged line.
    let mut seqs: Vec<u64> = Vec::new();
    for l in a.ok(&["ls-snapshot"]).lines().filter(|l| l.starts_with("task")) {
        let obj = a.ok(&["cat-object", l.split_whitespace().nth(1).unwrap()]);
        let v: serde_json::Value = serde_json::from_str(obj.splitn(2, '\n').nth(1).unwrap()).unwrap();
        seqs.push(v["seq"].as_u64().unwrap());
        for f in v["former"].as_array().into_iter().flatten() {
            seqs.push(f["seq"].as_u64().unwrap());
        }
    }
    let mut dedup = seqs.clone();
    dedup.sort();
    dedup.dedup();
    assert_eq!(dedup.len(), seqs.len(), "positions: {:?}", seqs);
    b.ok(&["add", "later"]);
    let plan = b.ok(&["plan"]);
    assert!(plan.lines().any(|l| l.starts_with("[ ] 5 t") && l.contains(" later")), "the counter is past every position: {}", plan);
}

#[test]
fn a_field_changed_on_both_sides_is_a_conflict_until_preferred() {
    let a = Proj::new("conflict-a");
    dana(&a);
    let b = Proj::new("conflict-b");
    copy_store(&a.dir, &b.dir);
    a.ok(&["edit", "1.1", "-b", "alice's body"]);
    a.ok(&["edit", "1.2", "--title", "Build a CLI (alice)"]);
    b.ok(&["edit", "1.1", "-b", "bob's body"]);
    b.ok(&["edit", "1.2", "-b", "a body only bob set"]);
    b.ok(&["unlink", "r2"]);
    a.ok(&["move", "r2", "--on", "1.3"]);
    bring_objects(&b.dir, &a.dir);
    let theirs = head(&b);
    let msg = a.fails(&["merge", &theirs]);
    assert!(msg.contains("conflicts") && msg.contains("1.1") && msg.contains("body"), "{}", msg);
    assert!(msg.contains("r2: ") && msg.contains("removed on one side") || msg.contains("r2: changed on one side, removed on the other"), "{}", msg);
    assert!(msg.contains("nothing was written"), "{}", msg);
    assert!(a.ok(&["show", "1.1"]).contains("alice's body"), "untouched after a refused merge");
    let out = a.ok(&["merge", &theirs, "--prefer", "theirs"]);
    assert!(out.contains("merged"), "{}", out);
    assert!(a.ok(&["show", "1.1"]).contains("bob's body"));
    let s12 = a.ok(&["show", "1.2"]);
    assert!(s12.contains("Build a CLI (alice)") && s12.contains("a body only bob set"), "disjoint fields merge: {}", s12);
    assert!(a.fails(&["show", "r2"]).contains("no task") || !a.ok(&["show", "1.3"]).contains("r2 "), "r2 removed as theirs preferred");
    assert!(a.ok(&["log", "-n", "1"]).contains("resolved preferring theirs"));
    // Status on both sides is reported, and theirs stands.
    let c = Proj::new("conflict-c");
    copy_store(&a.dir, &c.dir);
    a.ok(&["complete", "1.1"]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    c.ok(&["start", "1.1"]);
    bring_objects(&c.dir, &a.dir);
    let out = a.ok(&["merge", &head(&c)]);
    assert!(out.contains("status on both sides") && out.contains("the later stands"), "{}", out);
    assert!(a.ok(&["plan"]).contains("[>] 1.1"), "chronological: the later start stands");
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(["-c", "user.email=t@t", "-c", "user.name=t"]).args(args).current_dir(dir).output().expect("git on PATH");
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

#[test]
fn git_merges_resolve_by_markers_or_by_the_driver() {
    let origin = Proj::new("git-origin");
    git(&origin.dir, &["init", "-q"]);
    dana(&origin);
    git(&origin.dir, &["add", "-A"]);
    git(&origin.dir, &["commit", "-qm", "plan"]);
    let clone_dir = std::env::temp_dir().join(format!("tsk-test-git-clone-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&clone_dir);
    git(&origin.dir, &["clone", "-q", ".", clone_dir.to_str().unwrap()]);
    let clone = Proj { dir: clone_dir };
    // Both sides add, both commit.
    origin.ok(&["add", "ours"]);
    git(&origin.dir, &["add", "-A"]);
    git(&origin.dir, &["commit", "-qm", "ours"]);
    clone.ok(&["add", "theirs"]);
    git(&clone.dir, &["add", "-A"]);
    git(&clone.dir, &["commit", "-qm", "theirs"]);
    // Without the driver: git leaves markers in HEAD; the log merges as a union.
    // (A machine with `tsk git-setup` done would resolve it; force git's
    // own text merge for this phase, as a machine without the driver does.)
    std::fs::create_dir_all(clone.dir.join(".git/info")).unwrap();
    std::fs::write(clone.dir.join(".git/info/attributes"), ".tasks/HEAD merge=text\n").unwrap();
    git(&clone.dir, &["fetch", "-q"]);
    let m = git(&clone.dir, &["merge", "origin/master", "origin/main"]);
    let m = if m.contains("not something we can merge") { git(&clone.dir, &["merge", "origin/master"]) } else { m };
    let m = if m.contains("not something we can merge") { git(&clone.dir, &["merge", "origin/main"]) } else { m };
    assert!(m.contains("CONFLICT (content): Merge conflict in .tasks/HEAD"), "{}", m);
    assert!(clone.read(".tasks/HEAD").contains("<<<<<<<"));
    let out = clone.ok(&["merge"]);
    assert!(out.contains("merged") && out.contains("event(s) replayed"), "{}", out);
    let plan = clone.ok(&["plan"]);
    assert!(plan.contains(" theirs") && plan.contains(" ours"), "{}", plan);
    assert!(!clone.read(".tasks/HEAD").contains("<<<<<<<"));
    assert!(!clone.read(".tasks/log").contains("<<<<<<<"), "the log is rebuilt");
    git(&clone.dir, &["add", "-A"]);
    git(&clone.dir, &["commit", "-qm", "merged"]);
    assert!(git(&clone.dir, &["status", "--short"]).trim().is_empty());
    std::fs::remove_file(clone.dir.join(".git/info/attributes")).unwrap();
    // With the driver: the git merge is clean, HEAD carries the pending
    // merge, and the next tsk command performs it.
    let exe = env!("CARGO_BIN_EXE_tsk").replace('\\', "/");
    git(&clone.dir, &["config", "merge.tsk.driver", &format!("\"{}\" merge-driver %A %B", exe)]);
    origin.ok(&["add", "ours 2"]);
    git(&origin.dir, &["add", "-A"]);
    git(&origin.dir, &["commit", "-qm", "ours2"]);
    clone.ok(&["add", "theirs 2"]);
    git(&clone.dir, &["add", "-A"]);
    git(&clone.dir, &["commit", "-qm", "theirs2"]);
    git(&clone.dir, &["fetch", "-q"]);
    let branch = git(&origin.dir, &["rev-parse", "--abbrev-ref", "HEAD"]).trim().to_string();
    let m = git(&clone.dir, &["merge", &format!("origin/{}", branch)]);
    assert!(!m.contains("CONFLICT"), "{}", m);
    let hf = clone.read(".tasks/HEAD");
    assert!(hf.lines().nth(1).unwrap_or("").starts_with("merge "), "{}", hf);
    let (ok, text) = clone.run_from(&clone.dir, &[], &["plan"]);
    assert!(ok && text.contains("merged") && text.contains("theirs 2") && text.contains("ours 2"), "{}", text);
    assert_eq!(clone.read(".tasks/HEAD").lines().count(), 1, "the pending line is consumed");
    assert!(clone.ok(&["diff"]).contains("event(s)"), "the replay is uncommitted work the session commits");
    let _ = std::fs::remove_dir_all(&clone.dir);
}

#[test]
fn a_pending_merge_with_conflicts_blocks_writes_and_warns_reads() {
    let a = Proj::new("pending-a");
    dana(&a);
    let b = Proj::new("pending-b");
    copy_store(&a.dir, &b.dir);
    a.ok(&["edit", "1.1", "-b", "alice"]);
    b.ok(&["edit", "1.1", "-b", "bob"]);
    bring_objects(&b.dir, &a.dir);
    let (ours, theirs) = (head(&a), head(&b));
    std::fs::write(a.dir.join(".tasks/HEAD"), format!("{}\nmerge {}\n", ours, theirs)).unwrap();
    let (ok, text) = a.run(&["show", "1.1"]);
    assert!(ok && text.contains("pending") && text.contains("alice"), "reads warn and show our side: {}", text);
    let msg = a.fails(&["add", "another"]);
    assert!(msg.contains("pending") && msg.contains("--prefer"), "{}", msg);
    assert!(a.read(".tasks/HEAD").contains("merge "), "the pending line survives");
    let dry = a.fails(&["merge", "--dry-run"]);
    assert!(dry.contains("1.1") && dry.contains("body"), "{}", dry);
    a.ok(&["merge", "--prefer", "ours"]);
    assert!(a.ok(&["show", "1.1"]).contains("alice"));
    assert_eq!(a.read(".tasks/HEAD").lines().count(), 1);
    a.ok(&["add", "another"]);
}

#[test]
fn any_merge_can_be_undone_with_reset_and_the_reflog() {
    let a = Proj::new("undo-a");
    dana(&a);
    let b = Proj::new("undo-b");
    copy_store(&a.dir, &b.dir);
    a.ok(&["add", "alice's root"]);
    b.ok(&["add", "bob's root"]);
    b.ok(&["complete", "1.1"]);
    bring_objects(&b.dir, &a.dir);
    let before = head(&a);
    let out = a.ok(&["merge", &head(&b)]);
    assert!(out.contains("merged"), "{}", out);
    let after = head(&a);
    assert!(a.ok(&["log", "-n", "1"]).contains(&format!("(from {})", &before[..10])), "the merge event names the pre-merge HEAD");
    let reflog = a.ok(&["reflog"]);
    assert!(reflog.contains(&format!("HEAD was {}", &before[..10])), "{}", reflog);
    assert!(reflog.lines().next().unwrap().starts_with(&after[..10]), "{}", reflog);
    // Back to before the merge: the plan is ours alone again, nothing lost.
    let out = a.ok(&["reset", &before[..10]]);
    assert!(out.contains("HEAD is now at") && out.contains(&after[..10]), "{}", out);
    let plan = a.ok(&["plan", "-a"]);
    assert!(plan.contains("alice's root") && !plan.contains("bob's root") && plan.contains("[ ] 1.1"), "{}", plan);
    assert!(a.ok(&["log"]).lines().count() == a.ok(&["rev-list"]).lines().count(), "the log is rebuilt for the restored line");
    // And forward again, even after a prune: the reflog keeps the other line whole.
    a.ok(&["prune"]);
    a.ok(&["reset", &after]);
    assert!(a.ok(&["plan", "-a"]).contains("bob's root"));
    assert!(a.ok(&["reflog"]).lines().count() >= 4);
    assert!(!a.read(".tasks/.gitignore").is_empty() && a.read(".tasks/.gitignore").contains("reflog"), "the reflog is local");
    assert!(a.fails(&["reset", "0000"]).contains("no object"));
}
