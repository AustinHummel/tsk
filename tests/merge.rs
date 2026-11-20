//! 0.3: merging divergent copies of a store, by hand and through git.

mod common;
use common::{dana, Proj};
use std::path::Path;

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
    a.ok(&["adopt", "1", "t5", "--fit", "it belongs to Dana's work"]);
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
    // A status changed differently on both sides is not a conflict in the
    // canonical merge: its order is the clock's from either copy, so the
    // later event stands, and the merge says so.
    let c = Proj::new("conflict-c");
    copy_store(&a.dir, &c.dir);
    a.ok(&["complete", "1.1"]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    c.ok(&["start", "1.1"]);
    bring_objects(&c.dir, &a.dir);
    let out = a.ok(&["merge", &head(&c)]);
    assert!(out.contains("status on both sides: 1.1 t2 ") && out.contains(": ours completed at ") && out.contains(", theirs started at ") && out.contains("; the later stands (ends in-progress)"), "{}", out);
    assert!(a.ok(&["plan"]).contains("[>] 1.1"), "the later start stands");
    assert!(a.ok(&["log", "-n", "1"]).contains("status on both sides: 1.1 t2 "), "the merge event carries it");
}

/// An unkeyed store settles what git brought as the canonical merge; a
/// status changed on both sides is said there too.
#[test]
fn a_canonical_merge_through_the_pending_marker_reports_status_on_both_sides() {
    let (a, b) = pair("canon-marker-status", None);
    a.ok(&["complete", "1.1"]);
    later();
    b.ok(&["start", "1.1"]);
    bring_objects(&b.dir, &a.dir);
    let (ours, theirs) = (head(&a), head(&b));
    std::fs::write(a.dir.join(".tasks/HEAD"), format!("{}\nmerge {}\n", ours, theirs)).unwrap();
    let (ok, text) = a.run(&["plan"]);
    assert!(ok && text.contains("tsk: merged ") && text.contains("tsk: status on both sides: 1.1 t2 ") && text.contains("; the later stands (ends in-progress)"), "{}", text);
    assert!(text.contains("[>] 1.1"), "{}", text);
    assert_eq!(a.read(".tasks/HEAD").lines().count(), 1, "settled");
}

/// A complete merged both ways, then uncompleted on one copy: only that
/// copy changed the status since the two last met, so neither merge stops
/// or claims both sides changed it, and the copies agree.
#[test]
fn an_informed_uncomplete_merges_cleanly_from_both_copies() {
    let (a, b) = pair("informed", None);
    a.ok(&["complete", "1.1"]);
    b.ok(&["add", "bob's root"]);
    bring_objects(&b.dir, &a.dir);
    a.ok(&["merge", &head(&b)]);
    bring_objects(&a.dir, &b.dir);
    b.ok(&["merge", &head(&a)]);
    assert!(b.ok(&["plan", "-a"]).contains("[x] 1.1"));
    assert_ne!(head(&a), head(&b), "the copies' HEADs differ after merging both ways");
    later();
    b.ok(&["uncomplete", "1.1"]);
    later();
    a.ok(&["add", "alice's root"]);
    bring_objects(&b.dir, &a.dir);
    bring_objects(&a.dir, &b.dir);
    let (ha, hb) = (head(&a), head(&b));
    let out = a.ok(&["merge", &hb]);
    assert!(out.contains("merged") && !out.contains("status on both sides"), "{}", out);
    let out = b.ok(&["merge", &ha]);
    assert!(out.contains("merged") && !out.contains("status on both sides"), "{}", out);
    let (pa, pb) = (a.ok(&["plan", "-a"]), b.ok(&["plan", "-a"]));
    assert!(pa.contains("[ ] 1.1"), "the uncomplete stands: {}", pa);
    assert_eq!(pa, pb, "both copies end the same");
}

/// A status changed on both sides ends the same on both copies whichever
/// merges first, and `--prefer` does not move it.
#[test]
fn a_status_changed_on_both_sides_ends_the_same_whichever_copy_merges() {
    let (a, b) = pair("either-first", None);
    a.ok(&["complete", "1.1"]);
    later();
    b.ok(&["start", "1.1"]);
    bring_objects(&b.dir, &a.dir);
    bring_objects(&a.dir, &b.dir);
    let (ha, hb) = (head(&a), head(&b));
    let c = Proj::new("either-first-c");
    copy_store(&a.dir, &c.dir);
    a.ok(&["merge", &hb]);
    b.ok(&["merge", &ha]);
    c.ok(&["merge", &hb, "--prefer", "ours"]);
    let (pa, pb, pc) = (a.ok(&["plan", "-a"]), b.ok(&["plan", "-a"]), c.ok(&["plan", "-a"]));
    assert!(pa.contains("[>] 1.1"), "the later start stands: {}", pa);
    assert_eq!(pa, pb, "the same whichever copy merges");
    assert_eq!(pa, pc, "--prefer does not settle a status in the canonical merge");
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = common::scrubbed("git").args(["-c", "user.email=t@t", "-c", "user.name=t"]).args(args).current_dir(dir).output().expect("git on PATH");
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

#[test]
fn closed_and_reopened_replay_in_a_merge() {
    let a = Proj::new("close-a");
    a.ok(&["add", "Run", "--by", "A: b", "--ongoing", "what the business owes"]);
    a.ok(&["amend", "1", "Piece", "--fit", "p"]);
    a.ok(&["complete", "1.1"]);
    let b = Proj::new("close-b");
    copy_store(&a.dir, &b.dir);
    a.ok(&["close", "1", "done"]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    b.ok(&["amend", "1", "New", "--fit", "n"]);
    bring_objects(&b.dir, &a.dir);
    let out = a.ok(&["merge", &head(&b)]);
    assert!(out.contains("1 t1 Run: closed on ours at") && out.contains("then gained 1.2 t3 New on theirs; it is open again"), "{}", out);
    let plan = a.ok(&["plan"]);
    assert!(plan.contains("[>] 1 t1 Run") && plan.contains("[ ] 1.2 t3 New"), "{}", plan);
    assert!(a.ok(&["log", "-n", "4"]).contains("closed"), "the close is on the merged line");
    // A reopen on one side against a reopen and a close on the other: close
    // and reopen move status as complete and uncomplete do, and the later
    // stands.
    a.ok(&["complete", "1.2"]);
    a.ok(&["close", "1", "done again"]);
    let c = Proj::new("close-c");
    copy_store(&a.dir, &c.dir);
    a.ok(&["reopen", "1"]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    c.ok(&["reopen", "1"]);
    c.ok(&["close", "1", "shut"]);
    bring_objects(&c.dir, &a.dir);
    let out = a.ok(&["merge", &head(&c)]);
    assert!(out.contains("status on both sides: 1 t1 Run: ours reopened at") && out.contains(", theirs closed at ") && out.contains("the later stands"), "{}", out);
    assert!(a.ok(&["plan", "-a"]).contains("[x] 1 t1 Run"), "the later close stands");
}

// ----- 0.7: writer keys -----------------------------------------------------

#[test]
fn two_keyed_copies_merge_both_ways_and_no_id_changes() {
    let a = Proj::new("keys-a");
    dana(&a); // t1 to t4, bare
    a.ok(&["config", "ids", "keyed"]);
    let b = Proj::new("keys-b");
    copy_store(&a.dir, &b.dir);
    a.ok(&["key", "m"]);
    b.ok(&["key", "p"]);
    a.run_from(&a.dir, &[("TSK_AUTHOR", "alice")], &["add", "alice's root", "--by", "Alice: hers"]); // t5m r5m
    a.run_from(&a.dir, &[("TSK_AUTHOR", "alice")], &["amend", "1", "alice's piece", "--fit", "hers"]); // t6m
    b.run_from(&b.dir, &[("TSK_AUTHOR", "bob")], &["add", "bob's root", "--by", "Bob: his"]); // t5p r5p
    b.run_from(&b.dir, &[("TSK_AUTHOR", "bob")], &["amend", "1", "bob's piece", "--fit", "his"]); // t6p
    b.run_from(&b.dir, &[("TSK_AUTHOR", "bob")], &["link", "t6p", "1.3", "his before showing"]); // r6p
    let (ha, hb) = (head(&a), head(&b));
    bring_objects(&b.dir, &a.dir);
    bring_objects(&a.dir, &b.dir);
    let out_a = a.ok(&["merge", &hb]);
    let out_b = b.ok(&["merge", &ha]);
    for out in [&out_a, &out_b] {
        assert!(out.contains("merged") && !out.contains("renumbered"), "no id changes:\n{}", out);
    }
    let (pa, pb) = (a.ok(&["plan"]), b.ok(&["plan"]));
    assert_eq!(pa, pb, "both ways give the same plan");
    for id in ["t5m alice's root", "t6m alice's piece", "t5p bob's root", "t6p bob's piece", "t1 Help Dana"] {
        assert!(pa.contains(id), "{} in:\n{}", id, pa);
    }
    // No two siblings share a position: four children under 1, numbered 1.1 to 1.5.
    assert!(pa.contains("1.4 t6m") ^ pa.contains("1.4 t6p") && pa.contains("1.5 t6"), "{}", pa);
    for p in [&a, &b] {
        assert!(p.ok(&["show", "t6p"]).contains("r6p"));
        assert!(p.ok(&["show", "t5m"]).contains("r5m"));
    }
    // Minting carries on past every number and position drawn: t5p and t6p
    // took positions 7 and 8 from the counter.
    let next = a.ok(&["add", "next"]);
    assert!(next.contains(" t9m next"), "{}", next);
}

#[test]
fn the_same_key_on_two_copies_refuses_the_merge() {
    let a = Proj::new("samekey-a");
    dana(&a);
    a.ok(&["config", "ids", "keyed"]);
    a.ok(&["key", "m"]);
    let b = Proj::new("samekey-b");
    copy_store(&a.dir, &b.dir); // the key file too: a copy made by hand, not by git
    a.ok(&["add", "alice's root"]); // t5m
    b.ok(&["add", "bob's root"]); // t5m as well
    bring_objects(&b.dir, &a.dir);
    let before = a.read(".tasks/HEAD");
    for args in [vec!["merge", &head(&b), "--dry-run"], vec!["merge", &head(&b)]] {
        let msg = a.fails(&args);
        assert!(
            msg.contains("t5m was made on both copies, which share the key `m`") && msg.contains("`tsk key <letters>`") && msg.contains("`tsk reset ") && msg.lines().count() == 1,
            "{}",
            msg
        );
    }
    assert_eq!(a.read(".tasks/HEAD"), before, "nothing written");
    assert!(a.ok(&["plan"]).contains("t5m alice's root"));
}

// ----- 0.7: the shared-line merge --------------------------------------------

/// Two copies of Dana's plan. `shared` stands for the shared line (what was
/// pushed); `ours` holds work it has not published. Keyed when `keys` names
/// a key for each.
fn pair(name: &str, keys: Option<(&str, &str)>) -> (Proj, Proj) {
    let ours = Proj::new(&format!("{}-ours", name));
    dana(&ours);
    if keys.is_some() {
        ours.ok(&["config", "ids", "keyed"]);
    }
    let shared = Proj::new(&format!("{}-shared", name));
    copy_store(&ours.dir, &shared.dir);
    if let Some((o, s)) = keys {
        ours.ok(&["key", o]);
        shared.ok(&["key", s]);
    }
    (ours, shared)
}

/// "number id" for every task line of `tsk plan`, glyph and notes aside.
fn positions(plan: &str) -> Vec<String> {
    plan.lines()
        .filter_map(|l| {
            let w: Vec<&str> = l.split_once(']')?.1.split_whitespace().collect();
            (w.len() > 1).then(|| format!("{} {}", w[0], w[1]))
        })
        .collect()
}

fn later() {
    std::thread::sleep(std::time::Duration::from_millis(1100));
}

#[test]
fn a_published_id_and_number_survive_a_merge_with_an_older_unpublished_event() {
    let (a, b) = pair("older-unpub", None);
    // Ours is older: the canonical merge would put it first and rename theirs.
    a.ok(&["amend", "1", "alice's step", "--fit", "hers"]); // t5, 1.4
    later();
    b.ok(&["amend", "1", "bob's step", "--fit", "his"]); // t5, 1.4: published
    let published = b.ok(&["plan"]);
    assert!(published.contains("1.4 t5 bob's step"), "{}", published);
    bring_objects(&b.dir, &a.dir);
    let out = a.ok(&["merge", &head(&b), "--upstream"]);
    assert!(out.contains("renumbered: ours t5 is now t6") && !out.contains("theirs t5"), "only ours is renamed:\n{}", out);
    let plan = a.ok(&["plan"]);
    assert!(plan.contains("1.4 t5 bob's step") && plan.contains("1.5 t6 alice's step"), "{}", plan);
    // Every id and number the shared line published is where it was.
    for l in published.lines() {
        assert!(plan.contains(l.trim_end()), "{} moved:\n{}", l, plan);
    }
}

#[test]
fn theirs_keeps_every_hash() {
    let (a, b) = pair("keep-hash", Some(("p", "m")));
    a.ok(&["add", "pc's root", "--by", "PC: work"]);
    a.ok(&["complete", "1.1"]);
    b.ok(&["add", "mac's root", "--by", "Mac: work"]);
    b.ok(&["edit", "1.2", "-b", "mac's body"]);
    bring_objects(&b.dir, &a.dir);
    let ours_before: Vec<String> = a.ok(&["rev-list"]).lines().map(String::from).collect();
    let theirs: Vec<String> = b.ok(&["rev-list"]).lines().map(String::from).collect();
    a.ok(&["merge", &theirs[0], "--upstream"]);
    let merged: Vec<String> = a.ok(&["rev-list"]).lines().map(String::from).collect();
    // Their whole line, event for event, is the tail of ours.
    assert!(merged.ends_with(&theirs), "their line is kept:\nmerged {:?}\ntheirs {:?}", merged, theirs);
    // Then our two events, replayed in our order, then the merge event.
    assert_eq!(merged.len(), theirs.len() + 3, "{:?}", merged);
    let js: serde_json::Value = serde_json::from_str(&a.ok(&["--json", "log", "-n", "3"])).unwrap();
    assert_eq!(js[0]["event"]["kind"], "Merged", "{}", js);
    assert_eq!(js[1]["event"]["replayed_from"], ours_before[0].as_str(), "our complete, last");
    assert_eq!(js[2]["event"]["replayed_from"], ours_before[1].as_str(), "our add, first");
    let plan = a.ok(&["plan", "-a"]);
    assert!(plan.contains("[x] 1.1") && plan.contains("pc's root") && plan.contains("mac's root"), "{}", plan);
    assert!(a.ok(&["show", "1.2"]).contains("mac's body"));
}

#[test]
fn nothing_unpublished_is_a_fast_forward() {
    let (a, b) = pair("ff", Some(("p", "m")));
    a.ok(&["add", "pc's root", "--by", "PC: work"]);
    b.ok(&["add", "mac's root", "--by", "Mac: work"]);
    // The shared line takes our work first (the canonical merge, replayed
    // under new hashes), then moves on.
    bring_objects(&a.dir, &b.dir);
    b.ok(&["merge", &head(&a)]);
    b.ok(&["start", "1.1"]);
    bring_objects(&b.dir, &a.dir);
    let out = a.ok(&["merge", &head(&b), "--upstream"]);
    assert!(out.contains("fast-forward"), "every event of ours is on their line, replayed:\n{}", out);
    assert_eq!(head(&a), head(&b));
    assert_eq!(a.ok(&["plan", "-a"]), b.ok(&["plan", "-a"]));
}

#[test]
fn the_report_names_every_number_of_ours_that_moved() {
    let (a, b) = pair("moved", Some(("p", "m")));
    a.ok(&["add", "pc's root", "--by", "PC: work"]); // t5p, root 2
    a.ok(&["amend", "1", "pc's piece", "--fit", "hers"]); // t6p, 1.4
    later();
    b.ok(&["add", "mac's root", "--by", "Mac: work"]); // t5m, root 2
    b.ok(&["amend", "1", "mac's piece", "--fit", "his"]); // t6m, 1.4
    bring_objects(&b.dir, &a.dir);
    let json: serde_json::Value = serde_json::from_str(&a.ok(&["--json", "merge", &head(&b), "--upstream", "--dry-run"])).unwrap();
    let moved = json["moved"].as_array().unwrap_or_else(|| panic!("{}", json));
    assert!(moved.contains(&serde_json::json!({"id": "t5p", "from": "2", "to": "3"})), "{}", json);
    assert!(moved.contains(&serde_json::json!({"id": "t6p", "from": "1.4", "to": "1.5"})), "{}", json);
    assert_eq!(moved.len(), 2, "nothing else of ours moved: {}", json);
    let out = a.ok(&["merge", &head(&b), "--upstream"]);
    // Keyed ids keep their ids (the U3 case: the next position, not a new id).
    assert!(out.contains("moved: t5p 2 -> 3") && out.contains("moved: t6p 1.4 -> 1.5") && !out.contains("renumbered"), "{}", out);
    let plan = a.ok(&["plan"]);
    assert!(plan.contains("[ ] 2 t5m mac's root") && plan.contains("[ ] 3 t5p pc's root"), "{}", plan);
    assert!(plan.contains("1.4 t6m mac's piece") && plan.contains("1.5 t6p pc's piece"), "{}", plan);
    let log = a.ok(&["log", "-n", "1"]);
    assert!(log.contains("moved t5p 2->3") && log.contains("t6p 1.4->1.5"), "the merge event carries the report: {}", log);
}

#[test]
fn a_status_changed_on_both_sides_is_a_conflict_until_preferred() {
    // The blind review's case: a published complete, and an older start
    // not yet published, which replayed after it used to reopen the task.
    let (a, b) = pair("both-status", Some(("p", "m")));
    a.ok(&["start", "1.1"]); // older, unpublished
    later();
    b.ok(&["complete", "1.1"]); // newer, published
    bring_objects(&b.dir, &a.dir);
    let theirs = head(&b);
    let a2 = Proj::new("both-status-ours2");
    copy_store(&a.dir, &a2.dir);
    let (h, revs) = (head(&a), a.ok(&["rev-list"]));
    for args in [vec!["merge", theirs.as_str(), "--upstream"], vec!["merge", theirs.as_str(), "--upstream", "--dry-run"]] {
        let msg = a.fails(&args);
        assert!(
            msg.contains("conflicts") && msg.contains("1.1 t2 ") && msg.contains("status (ours in-progress, started at ") && msg.contains("; theirs complete, completed at "),
            "{}",
            msg
        );
        assert!(msg.contains("nothing was written") && msg.contains("--prefer ours or --prefer theirs"), "{}", msg);
        assert_eq!((head(&a), a.ok(&["rev-list"])), (h.clone(), revs.clone()), "nothing written");
    }
    assert!(a.ok(&["plan"]).contains("[>] 1.1"), "our side, untouched");
    // Theirs preferred: the published complete stands.
    let js: serde_json::Value = serde_json::from_str(&a.ok(&["--json", "merge", &theirs, "--upstream", "--prefer", "theirs"])).unwrap();
    assert_eq!(js["resolved"].as_u64(), Some(1), "{}", js);
    let bt = &js["both_touched"][0];
    assert_eq!((bt["id"].as_str(), bt["ends"].as_str(), bt["older_stands"].as_bool()), (Some("t2"), Some("complete"), Some(false)), "{}", js);
    assert!(a.ok(&["plan", "-a"]).contains("[x] 1.1"), "{}", a.ok(&["plan", "-a"]));
    let log = a.ok(&["log", "-n", "1"]);
    assert!(log.contains("t2 ends complete") && log.contains("theirs preferred") && log.contains("resolved preferring theirs"), "{}", log);
    let shared = b.ok(&["rev-list"]);
    assert!(a.ok(&["rev-list"]).ends_with(&shared), "the shared line kept");
    // Ours preferred: the start stands.
    let out = a2.ok(&["merge", &theirs, "--upstream", "--prefer", "ours"]);
    assert!(out.contains("status on both sides: t2 ends in-progress (ours started ") && out.contains(", theirs completed ") && out.contains("; ours preferred)"), "{}", out);
    assert!(a2.ok(&["plan"]).contains("[>] 1.1"));
}

#[test]
fn the_same_status_on_both_sides_merges_cleanly() {
    let (a, b) = pair("same-status", Some(("p", "m")));
    a.ok(&["complete", "1.1"]);
    later();
    b.ok(&["complete", "1.1"]);
    bring_objects(&b.dir, &a.dir);
    let out = a.ok(&["merge", &head(&b), "--upstream"]);
    assert!(out.contains("status on both sides: t2 ends complete (ours completed ") && out.contains("; the same on both sides)"), "{}", out);
    assert!(a.ok(&["plan", "-a"]).contains("[x] 1.1"));
    // The canonical merge of an unkeyed store: the same.
    let (c, d) = pair("same-status-canon", None);
    c.ok(&["start", "1.1"]);
    later();
    d.ok(&["start", "1.1"]);
    bring_objects(&d.dir, &c.dir);
    let out = c.ok(&["merge", &head(&d)]);
    assert!(out.contains("status on both sides: 1.1 t2 ") && out.contains("; the same on both sides"), "{}", out);
    assert!(c.ok(&["plan"]).contains("[>] 1.1"));
}

#[test]
fn merging_twice_changes_nothing() {
    let (a, b) = pair("twice", Some(("p", "m")));
    a.ok(&["add", "pc's root", "--by", "PC: work"]);
    b.ok(&["add", "mac's root", "--by", "Mac: work"]);
    bring_objects(&b.dir, &a.dir);
    let theirs = head(&b);
    a.ok(&["merge", &theirs, "--upstream"]);
    let (h, revs, plan, log) = (head(&a), a.ok(&["rev-list"]), a.ok(&["plan", "-a"]), a.read(".tasks/log"));
    for args in [vec!["merge", theirs.as_str(), "--upstream"], vec!["merge", theirs.as_str(), "--upstream", "--dry-run"]] {
        let out = a.ok(&args);
        assert!(out.contains("already merged") || out.contains("would replay 0 event(s)"), "{}", out);
        assert_eq!((head(&a), a.ok(&["rev-list"]), a.ok(&["plan", "-a"]), a.read(".tasks/log")), (h.clone(), revs.clone(), plan.clone(), log.clone()));
    }
    // A fast-forward merged twice: the second is a no-op too.
    let (c, d) = pair("twice-ff", None);
    d.ok(&["add", "theirs only"]);
    bring_objects(&d.dir, &c.dir);
    assert!(c.ok(&["merge", &head(&d), "--upstream"]).contains("fast-forward"));
    let h = head(&c);
    assert!(c.ok(&["merge", &head(&d), "--upstream"]).contains("already merged"));
    assert_eq!(head(&c), h);
}

#[test]
fn a_pending_marker_already_merged_is_cleared() {
    // Either merge: here the canonical one, on an unkeyed store.
    let (a, b) = pair("marker", None);
    a.ok(&["add", "alice's root"]);
    b.ok(&["add", "bob's root"]);
    bring_objects(&b.dir, &a.dir);
    let theirs = head(&b);
    a.ok(&["merge", &theirs]);
    let ours = head(&a);
    std::fs::write(a.dir.join(".tasks/HEAD"), format!("{}\nmerge {}\n", ours, theirs)).unwrap();
    a.ok(&["plan"]);
    assert_eq!(a.read(".tasks/HEAD"), format!("{}\n", ours), "the marker is cleared, HEAD unchanged");
    let (ok, text) = a.run(&["plan"]);
    assert!(ok && !text.contains("merged "), "nothing is re-announced: {}", text);
}

#[test]
fn apply_and_prune_then_merge() {
    let (a, b) = pair("apply-prune", Some(("p", "m")));
    a.ok(&["type", "add", "change", "A change"]);
    a.ok(&["type", "item", "change", "tested", "Covered by a test"]);
    a.ok(&["add", "pc's root", "--by", "PC: work"]); // t5p
    b.ok(&["add", "mac's root", "--by", "Mac: work"]); // t5m
    b.ok(&["amend", "1", "mac's piece", "--fit", "his"]); // t6m
    bring_objects(&b.dir, &a.dir);
    let first = head(&b);
    a.ok(&["merge", &first, "--upstream"]);
    // After a shared-line merge: apply a type, prune, and keep working.
    a.ok(&["apply", "change", "t5p"]);
    a.ok(&["prune"]);
    let out = a.ok(&["prune", "--before", &first]);
    assert!(out.contains("pruned") || out.contains("removed"), "{}", out);
    a.ok(&["start", "1.1"]);
    let plan = a.ok(&["plan", "-a"]);
    assert!(plan.contains("[ ] 2 t5m mac's root") && plan.contains("1.4 t6m mac's piece") && plan.contains("t5p pc's root"), "{}", plan);
    assert!(a.ok(&["show", "t5p"]).contains("change"), "the type is applied");
    // The shared line moves on; ours merges it again, over the prune.
    b.ok(&["complete", "1.4"]);
    bring_objects(&b.dir, &a.dir);
    let published = b.ok(&["plan", "-a"]);
    let out = a.ok(&["merge", &head(&b), "--upstream"]);
    assert!(out.contains("merged"), "{}", out);
    let theirs: Vec<String> = b.ok(&["rev-list"]).lines().map(String::from).collect();
    assert!(a.ok(&["rev-list"]).lines().map(String::from).collect::<Vec<_>>().ends_with(&theirs), "their line is kept over a prune");
    let plan = a.ok(&["plan", "-a"]);
    for np in positions(&published) {
        assert!(positions(&plan).contains(&np), "{} moved:\n{}", np, plan);
    }
    assert!(plan.contains("[>] 1.1") && plan.contains("[x] 1.4 t6m"), "{}", plan);
    // The shared line takes ours by fast-forward: one line on both copies.
    bring_objects(&a.dir, &b.dir);
    assert!(b.ok(&["merge", &head(&a), "--upstream"]).contains("fast-forward"));
    assert_eq!(a.ok(&["plan", "-a"]), b.ok(&["plan", "-a"]));
    // A prune past our own unpublished work takes the snapshots replay
    // needs: the merge refuses in one sentence and writes nothing.
    // (Two events of ours: the snapshot between them is on this copy alone.)
    a.ok(&["add", "pc's late root", "--by", "PC: late"]);
    a.ok(&["edit", "pc's late root", "-b", "later still"]);
    let kept = Proj::new("apply-prune-kept"); // what git still holds from before the prune
    copy_store(&a.dir, &kept.dir);
    a.ok(&["prune", "--before", &head(&a)]);
    b.ok(&["add", "mac's late root", "--by", "Mac: late"]);
    bring_objects(&b.dir, &a.dir);
    let before = a.read(".tasks/HEAD");
    let msg = a.fails(&["merge", &head(&b), "--upstream"]);
    assert!(msg.contains("lost a snapshot to a prune, so nothing was written") && msg.contains("-- .tasks/objects") && msg.trim().lines().count() == 1, "{}", msg);
    assert_eq!(a.read(".tasks/HEAD"), before);
    // The way out: the objects come back, and the merge goes through.
    bring_objects(&kept.dir, &a.dir);
    a.ok(&["merge", &head(&b), "--upstream"]);
    let plan = a.ok(&["plan"]);
    assert!(plan.contains("[ ] 4 t") && plan.contains("mac's late root") && plan.contains("[ ] 5 t") && plan.contains("pc's late root"), "{}", plan);
}

// ----- 0.7: the shared-line merge through real git ---------------------------

/// git as a machine with `tsk git-setup` done would run it, without touching
/// this machine's own git config: a global config file of its own, and on
/// PATH a directory whose `tsk` is the binary under test, so the driver
/// `git-setup` registers runs this build.
struct Machine {
    root: std::path::PathBuf,
    path: std::ffi::OsString,
}

impl Machine {
    fn new(name: &str) -> Machine {
        let root = std::env::temp_dir().join(format!("tsk-test-machine-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::copy(env!("CARGO_BIN_EXE_tsk"), root.join("bin").join(format!("tsk{}", std::env::consts::EXE_SUFFIX))).unwrap();
        std::fs::write(root.join("gitconfig"), "").unwrap();
        let mut dirs = vec![root.join("bin")];
        dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
        Machine { path: std::env::join_paths(dirs).unwrap(), root }
    }
    fn env(&self) -> Vec<(&'static str, String)> {
        vec![("GIT_CONFIG_GLOBAL", self.root.join("gitconfig").to_string_lossy().to_string()), ("PATH", self.path.to_string_lossy().to_string())]
    }
    /// Run git in `dir`; (success, stdout and stderr).
    fn git(&self, dir: &Path, args: &[&str]) -> (bool, String) {
        let mut cmd = common::scrubbed("git");
        cmd.args(["-c", "user.email=t@t", "-c", "user.name=t", "-c", "init.defaultBranch=main"]).args(args).current_dir(dir);
        for (k, v) in self.env() {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("git on PATH");
        (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    }
    fn ok(&self, dir: &Path, args: &[&str]) -> String {
        let (ok, text) = self.git(dir, args);
        assert!(ok, "git {:?}:\n{}", args, text);
        text
    }
    fn commit(&self, dir: &Path, msg: &str) {
        self.ok(dir, &["add", "-A"]);
        self.ok(dir, &["commit", "-qm", msg]);
    }
    /// tsk in `p` with this machine's environment.
    fn tsk(&self, p: &Proj, args: &[&str]) -> (bool, String) {
        let env = self.env();
        let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        p.run_from(&p.dir, &env, args)
    }
    /// A store under git with Dana's plan (keyed with key `m` when `keyed`),
    /// `git-setup` done, and a clone of it with key `p`: the shared line's
    /// home and a second machine's copy.
    fn pair(&self, name: &str, keyed: bool) -> (Proj, Proj) {
        let mac = Proj::new(&format!("{}-mac", name));
        self.ok(&mac.dir, &["init", "-q"]);
        dana(&mac);
        if keyed {
            mac.ok(&["config", "ids", "keyed"]);
            mac.ok(&["key", "m"]);
        }
        let (ok, text) = self.tsk(&mac, &["git-setup"]);
        assert!(ok, "{}", text);
        self.commit(&mac.dir, "plan");
        let pc = Proj { dir: std::env::temp_dir().join(format!("tsk-test-{}-pc-{}", name, std::process::id())) };
        let _ = std::fs::remove_dir_all(&pc.dir);
        self.ok(&mac.dir, &["clone", "-q", ".", pc.dir.to_str().unwrap()]);
        if keyed {
            pc.ok(&["key", "p"]);
        }
        (mac, pc)
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn the_driver_path_on_a_keyed_store_keeps_theirs() {
    let m = Machine::new("driver-keyed");
    let (mac, pc) = m.pair("driver-keyed", true);
    assert_eq!(m.ok(&pc.dir, &["config", "--global", "merge.tsk.driver"]).trim(), "tsk merge-driver %A %B %P", "git-setup registers %P");
    // The PC's work is older and unpublished; the Mac's is on the shared line.
    pc.ok(&["amend", "1", "pc's piece", "--fit", "hers"]); // t5p, 1.4
    m.commit(&pc.dir, "pc");
    later();
    mac.ok(&["amend", "1", "mac's piece", "--fit", "his"]); // t5m, 1.4
    mac.ok(&["add", "mac's root", "--by", "Mac: work"]); // t6m, 2
    m.commit(&mac.dir, "mac");
    let shared: Vec<String> = mac.ok(&["rev-list"]).lines().map(String::from).collect();
    m.ok(&pc.dir, &["fetch", "-q"]);
    let out = m.ok(&pc.dir, &["merge", "--no-edit", "origin/main"]);
    assert!(!out.contains("CONFLICT"), "{}", out);
    assert!(pc.read(".tasks/HEAD").lines().nth(1).unwrap_or("").starts_with("merge "), "the driver left the merge pending");
    let (ok, text) = m.tsk(&pc, &["plan"]);
    assert!(ok && text.contains("shared line kept") && text.contains("moved: t5p 1.4 -> 1.5"), "settled as the shared-line merge:\n{}", text);
    let merged: Vec<String> = pc.ok(&["rev-list"]).lines().map(String::from).collect();
    assert!(merged.ends_with(&shared), "the shared line, hash for hash:\nmerged {:?}\nshared {:?}", merged, shared);
    let plan = pc.ok(&["plan"]);
    assert!(plan.contains("1.4 t5m mac's piece") && plan.contains("1.5 t5p pc's piece") && plan.contains("2 t6m mac's root"), "{}", plan);
    assert_eq!(pc.read(".tasks/HEAD").lines().count(), 1);
    // Committed and pushed back, the Mac takes it as a fast-forward.
    m.commit(&pc.dir, "merged");
    m.ok(&mac.dir, &["pull", "--no-rebase", "--no-edit", "-q", pc.dir.to_str().unwrap(), "main"]);
    let (ok, text) = m.tsk(&mac, &["plan"]);
    assert!(ok && text.contains("1.5 t5p pc's piece"), "{}", text);
    assert_eq!(head(&mac), head(&pc));
    let _ = std::fs::remove_dir_all(&pc.dir);
}

#[test]
fn a_status_conflict_through_git_waits_for_prefer() {
    let m = Machine::new("driver-status");
    let (mac, pc) = m.pair("driver-status", true);
    pc.ok(&["start", "1.1"]); // older, unpublished
    m.commit(&pc.dir, "pc");
    later();
    mac.ok(&["complete", "1.1"]); // newer, on the shared line
    m.commit(&mac.dir, "mac");
    let shared = mac.ok(&["rev-list"]);
    m.ok(&pc.dir, &["fetch", "-q"]);
    let out = m.ok(&pc.dir, &["merge", "--no-edit", "origin/main"]);
    assert!(!out.contains("CONFLICT"), "{}", out);
    let (ok, text) = m.tsk(&pc, &["add", "another", "--by", "PC: more"]);
    assert!(!ok && text.contains("pending with 1 conflict(s)") && text.contains("--prefer ours|theirs"), "a write is refused:\n{}", text);
    assert!(pc.read(".tasks/HEAD").lines().nth(1).unwrap_or("").starts_with("merge "), "the pending marker is kept");
    let (ok, text) = m.tsk(&pc, &["plan"]);
    assert!(ok && text.contains("pending") && text.contains("[>] 1.1"), "a read warns and shows our side:\n{}", text);
    let (ok, text) = m.tsk(&pc, &["merge", "--dry-run"]);
    assert!(!ok && text.contains("1.1 t2 ") && text.contains("status (ours in-progress, started at ") && text.contains("theirs complete, completed at "), "{}", text);
    let (ok, text) = m.tsk(&pc, &["merge", "--prefer", "theirs"]);
    assert!(ok && text.contains("t2 ends complete") && text.contains("theirs preferred"), "{}", text);
    assert_eq!(pc.read(".tasks/HEAD").lines().count(), 1, "settled");
    assert!(pc.ok(&["plan", "-a"]).contains("[x] 1.1"));
    assert!(pc.ok(&["rev-list"]).ends_with(&shared), "the shared line, hash for hash");
    let _ = std::fs::remove_dir_all(&pc.dir);
}

#[test]
fn the_driver_refuses_during_a_rebase_on_a_keyed_store() {
    let m = Machine::new("driver-rebase");
    let (mac, pc) = m.pair("driver-rebase", true);
    pc.ok(&["add", "pc's root", "--by", "PC: work"]);
    m.commit(&pc.dir, "pc");
    let pc_head = head(&pc);
    mac.ok(&["add", "mac's root", "--by", "Mac: work"]);
    m.commit(&mac.dir, "mac");
    let mac_head = head(&mac);
    m.ok(&pc.dir, &["fetch", "-q"]);
    for backend in ["--merge", "--apply"] {
        let (ok, text) = m.git(&pc.dir, &["rebase", backend, "origin/main"]);
        assert!(!ok, "{}: {}", backend, text);
        assert!(
            text.contains("tsk: this store is keyed and git is rebasing, which swaps the sides, so the shared line would be replayed instead of kept; run `git rebase --abort`, then `git merge <the shared line>`"),
            "{}: {}",
            backend,
            text
        );
        // %A untouched: in a rebase it is the shared line's HEAD.
        assert_eq!(pc.read(".tasks/HEAD"), format!("{}\n", mac_head), "{}", backend);
        m.ok(&pc.dir, &["rebase", "--abort"]);
        assert_eq!(head(&pc), pc_head);
    }
    // The way out the sentence names.
    m.ok(&pc.dir, &["merge", "--no-edit", "origin/main"]);
    let (ok, text) = m.tsk(&pc, &["plan"]);
    assert!(ok && text.contains("shared line kept") && text.contains("moved: t5p 2 -> 3"), "{}", text);
    let _ = std::fs::remove_dir_all(&pc.dir);
    // An unkeyed store's driver is unchanged: the rebase goes through.
    let u = Machine::new("driver-rebase-unkeyed");
    let (umac, upc) = u.pair("driver-rebase-unkeyed", false);
    upc.ok(&["add", "pc's root"]);
    u.commit(&upc.dir, "pc");
    umac.ok(&["add", "mac's root"]);
    u.commit(&umac.dir, "mac");
    u.ok(&upc.dir, &["fetch", "-q"]);
    let out = u.ok(&upc.dir, &["rebase", "origin/main"]);
    assert!(!out.contains("CONFLICT"), "{}", out);
    assert!(upc.read(".tasks/HEAD").contains("\nmerge "), "{}", upc.read(".tasks/HEAD"));
    let (ok, text) = u.tsk(&upc, &["plan"]);
    assert!(ok && text.contains("pc's root") && text.contains("mac's root"), "{}", text);
    let _ = std::fs::remove_dir_all(&upc.dir);
}

// ----- 0.7: which side is the shared line, and claims a killed writer left ----

/// The shared line's git ref is `origin/main` unless the store names another.
/// A stash pop brings git's sides the other way round: %A is the shared line,
/// %B the stash. Settled as the driver recorded it, the stash would be kept
/// and the published events replayed after it, re-hashed and renumbered.
#[test]
fn a_stash_pop_keeps_the_shared_line_with_the_sides_swapped() {
    let m = Machine::new("stash-pop");
    let (mac, pc) = m.pair("stash-pop", true);
    // Two unpublished events on the PC, not committed.
    pc.ok(&["amend", "1", "pc's piece", "--fit", "hers"]); // t5p, 1.4
    pc.ok(&["add", "pc's root", "--by", "PC: work"]); // t6p, 2
    // The shared line moves on.
    later();
    mac.ok(&["amend", "1", "mac's piece", "--fit", "his"]); // t5m, 1.4
    mac.ok(&["add", "mac's root", "--by", "Mac: work"]); // t6m, 2
    m.commit(&mac.dir, "mac");
    let shared: Vec<String> = mac.ok(&["rev-list"]).lines().map(String::from).collect();
    let published = positions(&mac.ok(&["plan"]));
    m.ok(&pc.dir, &["stash", "push", "-u", "-m", "tsk-test-stash"]);
    m.ok(&pc.dir, &["pull", "--no-rebase", "--no-edit", "-q"]);
    m.ok(&pc.dir, &["stash", "pop"]);
    assert!(pc.read(".tasks/HEAD").contains("\nmerge "), "the driver left the merge pending: {}", pc.read(".tasks/HEAD"));
    let (ok, text) = m.tsk(&pc, &["plan"]);
    assert!(ok && text.contains("sides swapped"), "the swap is reported:\n{}", text);
    let merged: Vec<String> = pc.ok(&["rev-list"]).lines().map(String::from).collect();
    assert!(merged.ends_with(&shared), "the shared line, hash for hash:\nmerged {:?}\nshared {:?}", merged, shared);
    let plan = positions(&pc.ok(&["plan"]));
    for p in &published {
        assert!(plan.contains(p), "published {} kept its number: {:?}", p, plan);
    }
    assert!(plan.contains(&"1.5 t5p".to_string()) && plan.contains(&"3 t6p".to_string()), "{:?}", plan);
    // Committed and taken back, the shared line's home keeps every number.
    m.commit(&pc.dir, "merged");
    m.ok(&mac.dir, &["pull", "--no-rebase", "--no-edit", "-q", pc.dir.to_str().unwrap(), "main"]);
    let back = positions(&mac.ok(&["plan"]));
    for p in &published {
        assert!(back.contains(p), "published {} kept its number on the Mac: {:?}", p, back);
    }
    assert_eq!(head(&mac), head(&pc));
}

/// A cherry-pick also brings the sides the other way round.
#[test]
fn a_cherry_pick_keeps_the_shared_line_with_the_sides_swapped() {
    let m = Machine::new("cherry-pick");
    let (mac, pc) = m.pair("cherry-pick", true);
    m.ok(&pc.dir, &["checkout", "-q", "-b", "side"]);
    pc.ok(&["add", "pc's root", "--by", "PC: work"]); // t5p
    m.commit(&pc.dir, "side work");
    m.ok(&pc.dir, &["checkout", "-q", "main"]);
    later();
    mac.ok(&["add", "mac's root", "--by", "Mac: work"]); // t5m, 2
    mac.ok(&["add", "mac's second", "--by", "Mac: work"]); // t6m, 3
    m.commit(&mac.dir, "mac");
    let shared: Vec<String> = mac.ok(&["rev-list"]).lines().map(String::from).collect();
    m.ok(&pc.dir, &["pull", "--no-rebase", "--no-edit", "-q"]);
    let out = m.ok(&pc.dir, &["cherry-pick", "side"]);
    assert!(!out.contains("CONFLICT"), "{}", out);
    let (ok, text) = m.tsk(&pc, &["plan"]);
    assert!(ok && text.contains("sides swapped"), "{}", text);
    let merged: Vec<String> = pc.ok(&["rev-list"]).lines().map(String::from).collect();
    assert!(merged.ends_with(&shared), "merged {:?}\nshared {:?}", merged, shared);
    let plan = positions(&pc.ok(&["plan"]));
    for p in ["2 t5m", "3 t6m", "4 t5p"] {
        assert!(plan.contains(&p.to_string()), "{}: {:?}", p, plan);
    }
}

/// When the shared line's head can't be read, or neither side holds it, the
/// merge is refused, nothing written, with the way out; a read shows our side.
#[test]
fn a_merge_whose_shared_line_cannot_be_told_is_refused() {
    let m = Machine::new("no-shared");
    let (mac, pc) = m.pair("no-shared", true);
    pc.ok(&["add", "pc's root", "--by", "PC: work"]);
    m.commit(&pc.dir, "pc");
    mac.ok(&["add", "mac's root", "--by", "Mac: work"]);
    m.commit(&mac.dir, "mac");
    let mac_head = head(&mac);
    // The Mac is the shared line's home: it has no origin/main.
    m.ok(&mac.dir, &["pull", "--no-rebase", "--no-edit", "-q", pc.dir.to_str().unwrap(), "main"]);
    let marker = mac.read(".tasks/HEAD");
    assert!(marker.contains("\nmerge "), "{}", marker);
    let (ok, text) = m.tsk(&mac, &["add", "more"]);
    assert!(
        !ok && text.contains("not committed at origin/main") && text.contains("nothing was written") && text.contains("git config tsk.shared <ref>"),
        "{}",
        text
    );
    assert_eq!(mac.read(".tasks/HEAD"), marker, "nothing written");
    let (ok, text) = m.tsk(&mac, &["plan"]);
    assert!(ok && text.contains("showing our side") && text.contains("2 t5m"), "a read is not blocked:\n{}", text);
    assert_eq!(mac.read(".tasks/HEAD"), marker, "nothing written");
    // The way out: name this copy's shared line, its own main (never
    // committed). main's committed HEAD is the pull's marker, so its settled
    // base is read from git's reflog: the Mac's own head before the pull.
    m.ok(&mac.dir, &["config", "tsk.shared", "main"]);
    let (ok, text) = m.tsk(&mac, &["plan"]);
    assert!(ok && text.contains("sides swapped"), "{}", text);
    let plan = positions(&mac.ok(&["plan"]));
    assert!(plan.contains(&"2 t5m".to_string()) && plan.contains(&"3 t5p".to_string()), "{:?}", plan);
    assert!(mac.ok(&["rev-list"]).contains(&mac_head));
}

/// A pid no process holds any more: a child that has exited.
fn dead_pid() -> u32 {
    let mut child = common::scrubbed("git").arg("--version").stdout(std::process::Stdio::null()).spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

fn backdate(path: &Path, secs: u64) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(secs)).unwrap();
}

/// A writer killed after claiming our HEAD and before moving it (here, in
/// the middle of settling a merge) leaves the claim and objects behind.
/// HEAD still names the claimed parent and the claim's process is gone, so
/// the claim guards nothing: the next command goes on. A claim whose
/// process is alive still refuses a write, naming the file, and never
/// blocks a read.
#[test]
fn a_claim_left_by_a_killed_writer_does_not_wedge_the_store() {
    let (a, b) = pair("killed", None);
    a.ok(&["add", "alice's root"]);
    b.ok(&["add", "bob's root"]);
    bring_objects(&b.dir, &a.dir);
    let (ours, theirs) = (head(&a), head(&b));
    std::fs::write(a.dir.join(".tasks/HEAD"), format!("{}\nmerge {}\n", ours, theirs)).unwrap();
    std::fs::create_dir_all(a.dir.join(".tasks/claims")).unwrap();
    std::fs::write(a.dir.join(".tasks/claims").join(&ours), format!("pid {}\ntime 2026-11-19T00:00:00Z\n", dead_pid())).unwrap();
    let (ok, text) = a.run(&["plan"]);
    assert!(ok && text.contains("merged "), "the read settles the merge:\n{}", text);
    assert_eq!(a.read(".tasks/HEAD").lines().count(), 1);
    assert_ne!(head(&a), ours);
    a.ok(&["add", "after"]);
    a.ok(&["reset", &ours]);
    a.ok(&["prune"]);

    // A live writer's claim on HEAD: a write is refused, naming the file;
    // a read shows our side.
    let (c, d) = pair("live-claim", None);
    c.ok(&["add", "alice's root"]);
    d.ok(&["add", "bob's root"]);
    bring_objects(&d.dir, &c.dir);
    let (ours, theirs) = (head(&c), head(&d));
    let marker = format!("{}\nmerge {}\n", ours, theirs);
    std::fs::write(c.dir.join(".tasks/HEAD"), &marker).unwrap();
    std::fs::create_dir_all(c.dir.join(".tasks/claims")).unwrap();
    let claim = c.dir.join(".tasks/claims").join(&ours);
    let live = common::LiveTsk::new();
    let exe = Path::new(env!("CARGO_BIN_EXE_tsk")).file_name().unwrap().to_string_lossy().to_string();
    std::fs::write(&claim, format!("pid {}\nexe {}\ntime 2026-11-19T00:00:00Z\n", live.pid(), exe)).unwrap();
    let (ok, text) = c.run(&["add", "more"]);
    assert!(!ok && text.contains("nothing was written") && text.contains(&format!(".tasks/claims/{}", ours)), "{}", text);
    assert_eq!(c.read(".tasks/HEAD"), marker);
    let (ok, text) = c.run(&["plan"]);
    assert!(ok && text.contains("showing our side"), "{}", text);
    assert_eq!(c.read(".tasks/HEAD"), marker);
    // The way out the sentence names.
    std::fs::remove_file(&claim).unwrap();
    c.ok(&["add", "more"]);
    assert_eq!(c.read(".tasks/HEAD").lines().count(), 1);
    drop(live);
}

/// `git checkout` back to a store HEAD that was already built on: the claim
/// from the line left behind no longer refuses every write. 0.6.0 left its
/// claims empty; one a minute old counts as left behind too.
#[test]
fn a_checkout_back_to_a_built_on_head_still_takes_writes() {
    let m = Machine::new("checkout-back");
    let (mac, _pc) = m.pair("checkout-back", false);
    let x = head(&mac);
    mac.ok(&["add", "on the line left behind"]);
    m.ok(&mac.dir, &["checkout", "--", ".tasks/HEAD"]);
    assert_eq!(head(&mac), x);
    mac.ok(&["add", "after the checkout"]);
    m.ok(&mac.dir, &["checkout", "--", ".tasks/HEAD"]);
    // As 0.6.0 left them: empty files, a minute old.
    for e in std::fs::read_dir(mac.dir.join(".tasks/claims")).unwrap().flatten() {
        std::fs::write(e.path(), "").unwrap();
        backdate(&e.path(), 120);
    }
    mac.ok(&["add", "after a 0.6.0 claim"]);
    let plan = mac.ok(&["plan"]);
    assert!(plan.contains("after a 0.6.0 claim") && !plan.contains("after the checkout"), "{}", plan);
}

/// A claim whose pid now belongs to another program (the pid was reused
/// after its writer exited) guards nothing: the claim names the writer's
/// program (`exe`), or, from before it did, any program called tsk.
#[test]
fn a_claim_whose_pid_was_reused_does_not_wedge_writes() {
    let (p, _) = pair("pid-reused", None);
    let exe = Path::new(env!("CARGO_BIN_EXE_tsk")).file_name().unwrap().to_string_lossy().to_string();
    // This test's own process is alive, and is not tsk.
    for text in [format!("pid {}\nexe {}\ntime 2026-11-19T00:00:00Z\n", std::process::id(), exe), format!("pid {}\ntime 2026-11-19T00:00:00Z\n", std::process::id())] {
        let h = head(&p);
        std::fs::create_dir_all(p.dir.join(".tasks/claims")).unwrap();
        std::fs::write(p.dir.join(".tasks/claims").join(&h), text).unwrap();
        let (ok, out) = p.run(&["add", "after a reused pid"]);
        assert!(ok, "{}", out);
        assert_ne!(head(&p), h);
    }
}

// ----- 0.7: a second merge over a pending one --------------------------------

/// A side branch on both machines: the Mac's copy is the shared line's home
/// (`origin` for the PC), and the PC works on `side/x` tracking
/// `origin/side/x`.
fn side_branch(m: &Machine, name: &str) -> (Proj, Proj) {
    let (mac, pc) = m.pair(name, true);
    m.ok(&mac.dir, &["branch", "side/x"]);
    m.ok(&pc.dir, &["fetch", "-q"]);
    m.ok(&pc.dir, &["checkout", "-q", "-b", "side/x", "origin/side/x"]);
    (mac, pc)
}

/// On the Mac (the origin), commit one event on `branch` and come back to
/// main. One copy minting on two branches would mint the same id on both,
/// so main's event is made under its own key, as a third copy would.
fn mac_commits(m: &Machine, mac: &Proj, branch: &str, title: &str) {
    m.ok(&mac.dir, &["checkout", "-q", branch]);
    mac.ok(&["key", if branch == "main" { "n" } else { "m" }]);
    mac.ok(&["add", title, "--by", "Mac: work"]);
    mac.ok(&["key", "m"]);
    m.commit(&mac.dir, title);
    m.ok(&mac.dir, &["checkout", "-q", "main"]);
}

/// The blind review's case: the PC, with an unpublished event, pulls its side
/// branch while origin/main is ahead of both sides, then merges origin/main
/// before any tsk command. The driver records both lines in .tasks/HEAD and
/// stops git; committing that, as git's hint says (the third review's
/// reproduction), loses neither: tsk settles them one at a time.
#[test]
fn a_second_merge_over_a_pending_one_is_recorded_and_committing_loses_nothing() {
    let m = Machine::new("second-merge");
    let (mac, pc) = side_branch(&m, "second-merge");
    pc.ok(&["add", "pc's root", "--by", "PC: work"]);
    m.commit(&pc.dir, "pc");
    mac_commits(&m, &mac, "side/x", "mac's side");
    mac_commits(&m, &mac, "main", "mac's main");
    m.ok(&pc.dir, &["pull", "--no-rebase", "--no-edit", "-q"]);
    let marker = pc.read(".tasks/HEAD");
    assert!(marker.contains("\nmerge "), "{}", marker);
    let (ok, text) = m.git(&pc.dir, &["merge", "--no-edit", "origin/main"]);
    assert!(!ok && text.contains("records 2 lines to merge") && text.contains("nothing is lost"), "{}", text);
    let recorded = pc.read(".tasks/HEAD");
    assert!(recorded.starts_with(&marker) && recorded.matches("merge ").count() == 2, "both lines:\n{}", recorded);
    m.commit(&pc.dir, "merge origin/main");
    let (ok, text) = m.git(&pc.dir, &["merge", "--no-edit", "origin/main"]);
    assert!(ok && text.contains("Already up to date"), "{}", text);
    let (ok, text) = m.tsk(&pc, &["add", "after", "--by", "PC: more"]);
    assert!(ok && text.contains("shared line kept"), "{}", text);
    let plan = pc.ok(&["plan"]);
    for t in ["mac's side", "mac's main", "pc's root", "after"] {
        assert!(plan.contains(t), "{} is on the line:\n{}", t, plan);
    }
    let main_line = m.ok(&mac.dir, &["show", "main:.tasks/HEAD"]).trim().to_string();
    assert!(pc.ok(&["rev-list"]).contains(&main_line), "origin/main's line, hash for hash");
    assert!(!pc.read(".tasks/HEAD").contains("merge "), "both settled");
}

/// A pending status conflict holds the marker: a second pull over it records
/// its line too; once the first is settled with --prefer, the second settles
/// at the next command, and everything is on the line.
#[test]
fn a_second_pull_over_a_pending_status_conflict_keeps_both_lines() {
    let m = Machine::new("second-pull-status");
    let (mac, pc) = m.pair("second-pull-status", true);
    pc.ok(&["start", "1.1"]);
    m.commit(&pc.dir, "pc");
    later();
    mac.ok(&["complete", "1.1"]);
    m.commit(&mac.dir, "mac");
    m.ok(&pc.dir, &["pull", "--no-rebase", "--no-edit", "-q"]);
    let (ok, text) = m.tsk(&pc, &["add", "another", "--by", "PC: more"]);
    assert!(!ok && text.contains("pending with 1 conflict(s)"), "{}", text);
    let marker = pc.read(".tasks/HEAD");
    mac.ok(&["add", "mac's more", "--by", "Mac: work"]);
    m.commit(&mac.dir, "mac more");
    let (ok, text) = m.git(&pc.dir, &["pull", "--no-rebase", "--no-edit"]);
    assert!(!ok && text.contains("records 2 lines to merge"), "{}", text);
    assert!(pc.read(".tasks/HEAD").starts_with(&marker), "the pending line is kept");
    m.commit(&pc.dir, "second pull");
    let (ok, text) = m.tsk(&pc, &["add", "another", "--by", "PC: more"]);
    assert!(!ok && text.contains("pending with 1 conflict(s)"), "the first is still a conflict:\n{}", text);
    let (ok, text) = m.tsk(&pc, &["merge", "--prefer", "theirs"]);
    assert!(ok && text.contains("theirs preferred"), "{}", text);
    assert!(pc.read(".tasks/HEAD").contains("merge "), "the second line is carried");
    let (ok, text) = m.tsk(&pc, &["plan", "-a"]);
    assert!(ok && text.contains("mac's more") && text.contains("[x] 1.1"), "{}", text);
    assert!(pc.ok(&["rev-list"]).ends_with(&mac.ok(&["rev-list"])), "the shared line, hash for hash");
}

/// The routine flows settle with no refusal and nothing lost: a rejected
/// push on a side branch fixed by merging its upstream while origin/main is
/// ahead of both sides, then a landing's merge of main into the side branch
/// and the fast-forward of main.
#[test]
fn a_rejected_push_and_a_landing_settle_without_a_refusal() {
    let m = Machine::new("routine");
    let (mac, pc) = side_branch(&m, "routine");
    pc.ok(&["add", "pc's root", "--by", "PC: work"]);
    m.commit(&pc.dir, "pc");
    mac_commits(&m, &mac, "side/x", "mac's side");
    mac_commits(&m, &mac, "main", "mac's main");
    let (ok, text) = m.git(&pc.dir, &["push", "-q", "origin", "side/x"]);
    assert!(!ok, "the push is rejected: {}", text);
    m.ok(&pc.dir, &["fetch", "-q"]);
    m.ok(&pc.dir, &["merge", "--no-edit", "origin/side/x"]);
    let (ok, text) = m.tsk(&pc, &["plan"]);
    assert!(ok && text.contains("shared line kept") && text.contains("mac's side") && text.contains("pc's root"), "{}", text);
    let side_line = m.ok(&mac.dir, &["show", "side/x:.tasks/HEAD"]).trim().to_string();
    assert!(pc.ok(&["rev-list"]).contains(&side_line), "origin/side/x's line, hash for hash");
    m.commit(&pc.dir, "settle the store");
    // The landing: main brought up to date, merged into the side branch,
    // then fast-forwarded to it.
    m.ok(&pc.dir, &["checkout", "-q", "main"]);
    m.ok(&pc.dir, &["merge", "-q", "--ff-only", "origin/main"]);
    m.ok(&pc.dir, &["checkout", "-q", "side/x"]);
    m.ok(&pc.dir, &["merge", "--no-edit", "main"]);
    let (ok, text) = m.tsk(&pc, &["plan"]);
    assert!(ok && text.contains("shared line kept"), "{}", text);
    for t in ["mac's side", "mac's main", "pc's root"] {
        assert!(text.contains(t), "{} is on the line:\n{}", t, text);
    }
    let main_line = m.ok(&mac.dir, &["show", "main:.tasks/HEAD"]).trim().to_string();
    assert!(pc.ok(&["rev-list"]).contains(&main_line), "main's line, hash for hash");
    m.commit(&pc.dir, "landed");
    m.ok(&pc.dir, &["checkout", "-q", "main"]);
    m.ok(&pc.dir, &["merge", "-q", "--ff-only", "side/x"]);
    let (ok, text) = m.tsk(&pc, &["plan"]);
    assert!(ok && !text.contains("pending") && text.contains("mac's side") && text.contains("pc's root") && text.contains("mac's main"), "{}", text);
}

/// In a swapped merge (a stash pop), the conflict and --prefer name the
/// sides as git does: ours is HEAD, the shared line; theirs is the stash.
#[test]
fn a_swapped_merge_names_the_sides_as_git_does() {
    let m = Machine::new("swap-labels");
    let (mac, pc) = m.pair("swap-labels", true);
    pc.ok(&["start", "1.1"]); // unpublished, not committed
    later();
    mac.ok(&["complete", "1.1"]);
    m.commit(&mac.dir, "mac");
    m.ok(&pc.dir, &["stash", "push", "-u", "-m", "tsk-test-stash"]);
    m.ok(&pc.dir, &["pull", "--no-rebase", "--no-edit", "-q"]);
    m.ok(&pc.dir, &["stash", "pop"]);
    let (ok, text) = m.tsk(&pc, &["merge", "--dry-run"]);
    assert!(
        !ok && text.contains("sides swapped: the shared line (origin/main) was our HEAD") && text.contains("status (ours complete, completed at ") && text.contains("theirs in-progress, started at "),
        "ours is HEAD:\n{}",
        text
    );
    let (ok, text) = m.tsk(&pc, &["merge", "--prefer", "ours"]);
    assert!(ok && text.contains("t2 ends complete") && text.contains("ours preferred"), "{}", text);
    assert!(pc.ok(&["plan", "-a"]).contains("[x] 1.1"), "the published complete stands");
    assert!(pc.ok(&["rev-list"]).ends_with(&mac.ok(&["rev-list"])), "the shared line, hash for hash");
}

/// A HEAD file that names more than one line to merge (git's markers over a
/// pending merge, on a machine without the driver) settles them one at a
/// time; a merge named over a pending one settles and carries the other.
#[test]
fn two_lines_to_merge_are_settled_one_at_a_time() {
    let (a, b) = pair("two-to-merge", None);
    a.ok(&["add", "alice's root"]);
    b.ok(&["add", "bob's root"]);
    let x = head(&b);
    b.ok(&["add", "bob's second"]);
    bring_objects(&b.dir, &a.dir);
    let (ours, theirs) = (head(&a), head(&b));
    let text = format!("<<<<<<< HEAD\n{}\nmerge {}\n=======\n{}\n>>>>>>> other\n", ours, x, theirs);
    std::fs::write(a.dir.join(".tasks/HEAD"), &text).unwrap();
    let (ok, out) = a.run(&["add", "more"]);
    assert!(ok, "{}", out);
    let plan = a.ok(&["plan"]);
    for t in ["alice's root", "bob's root", "bob's second", "more"] {
        assert!(plan.contains(t), "{}:\n{}", t, plan);
    }
    assert!(!a.read(".tasks/HEAD").contains("merge"), "{}", a.read(".tasks/HEAD"));
    // A merge named over a pending one: the pending line is carried.
    let (c, d) = pair("two-to-merge-named", None);
    c.ok(&["add", "carol's root"]);
    d.ok(&["add", "dave's root"]);
    let x = head(&d);
    d.ok(&["add", "dave's second"]);
    bring_objects(&d.dir, &c.dir);
    let (ours, theirs) = (head(&c), head(&d));
    std::fs::write(c.dir.join(".tasks/HEAD"), format!("{}\nmerge {}\n", ours, x)).unwrap();
    let (ok, out) = c.run(&["merge", &theirs]);
    assert!(ok, "{}", out);
    assert!(c.read(".tasks/HEAD").contains(&format!("merge {}", x)), "carried:\n{}", c.read(".tasks/HEAD"));
    let (ok, out) = c.run(&["plan"]);
    assert!(ok && out.contains("already merged") && out.contains("dave's second"), "{}", out);
    assert!(!c.read(".tasks/HEAD").contains("merge"), "{}", c.read(".tasks/HEAD"));
}

// ----- 0.7: the shared line at a ref that holds a marker ------------------

/// `side.sh land` with a push before any tsk command: the side branch merges
/// main in (the marker's ours is the side's line), main is fast-forwarded to
/// it and pushed. origin/main's committed HEAD then names the unsettled
/// merge, and its first line is the side's: it must not decide. Its settled
/// base (git's reflog) is main's published line, which is kept.
#[test]
fn a_pushed_landing_marker_does_not_swap_the_sides() {
    let m = Machine::new("pushed-marker");
    let (mac, pc) = m.pair("pushed-marker", true);
    m.ok(&mac.dir, &["branch", "side/x"]);
    pc.ok(&["add", "pc one", "--by", "PC: work"]);
    pc.ok(&["add", "pc two", "--by", "PC: work"]);
    m.commit(&pc.dir, "pc");
    m.ok(&mac.dir, &["checkout", "-q", "--detach"]);
    m.ok(&pc.dir, &["push", "-q", "origin", "main"]);
    m.ok(&mac.dir, &["checkout", "-q", "main"]);
    mac_commits(&m, &mac, "side/x", "mac's side");
    m.ok(&mac.dir, &["checkout", "-q", "--detach"]);
    m.ok(&pc.dir, &["fetch", "-q"]);
    m.ok(&pc.dir, &["checkout", "-q", "-b", "side/x", "origin/side/x"]);
    m.ok(&pc.dir, &["merge", "--no-edit", "main"]);
    m.ok(&pc.dir, &["checkout", "-q", "main"]);
    m.ok(&pc.dir, &["merge", "-q", "--ff-only", "side/x"]);
    m.ok(&pc.dir, &["push", "-q", "origin", "main"]);
    assert!(m.ok(&pc.dir, &["show", "origin/main:.tasks/HEAD"]).contains("\nmerge "), "origin/main holds the marker");
    let (ok, text) = m.tsk(&pc, &["plan"]);
    assert!(ok && text.contains("shared line kept") && !text.contains("sides swapped") && !text.contains("moved: t5p"), "{}", text);
    let plan = positions(&pc.ok(&["plan"]));
    for p in ["2 t5p", "3 t6p", "4 t5m"] {
        assert!(plan.contains(&p.to_string()), "{}: {:?}", p, plan);
    }
}

/// The copy that publishes holds main ahead of origin/main. Settling a
/// landing's merge on the side branch, whose upstream holds the side's own
/// line, must keep main's unpushed events where they are: main, the local
/// branch origin/main tracks, is asked before the branch's upstream.
#[test]
fn the_publishers_unpushed_main_is_never_renumbered() {
    let m = Machine::new("publisher");
    let (mac, pc) = m.pair("publisher", true);
    pc.ok(&["add", "pc one", "--by", "PC: work"]);
    m.commit(&pc.dir, "pc one");
    m.ok(&mac.dir, &["checkout", "-q", "--detach"]);
    m.ok(&pc.dir, &["push", "-q", "origin", "main"]);
    m.ok(&mac.dir, &["checkout", "-q", "main"]);
    m.ok(&mac.dir, &["branch", "side/x"]);
    pc.ok(&["add", "pc two, unpushed", "--by", "PC: work"]);
    m.commit(&pc.dir, "pc two");
    mac_commits(&m, &mac, "side/x", "mac's side");
    m.ok(&pc.dir, &["fetch", "-q"]);
    m.ok(&pc.dir, &["checkout", "-q", "-b", "side/x", "origin/side/x"]);
    m.ok(&pc.dir, &["merge", "--no-edit", "main"]);
    let (ok, text) = m.tsk(&pc, &["plan"]);
    assert!(ok && !text.contains("sides swapped") && !text.contains("moved: t6p"), "{}", text);
    let plan = positions(&pc.ok(&["plan"]));
    for p in ["2 t5p", "3 t6p", "4 t6m"] {
        assert!(plan.contains(&p.to_string()), "{}: {:?}", p, plan);
    }
}

/// A `ps` that cannot answer (busybox has no `-p`: it lists nothing) cannot
/// tell a live writer from an exited one, so its claim counts as alive: the
/// write is refused, nothing written, naming the file to remove.
#[cfg(unix)]
#[test]
fn a_ps_that_cannot_answer_counts_a_writer_as_alive() {
    use std::os::unix::fs::PermissionsExt;
    let (p, _) = pair("ps-blind", None);
    let bin = p.dir.join("fakebin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("ps"), "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(bin.join("ps"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut dirs = vec![bin.clone()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
    let path = std::env::join_paths(dirs).unwrap().to_string_lossy().to_string();
    let h = head(&p);
    std::fs::create_dir_all(p.dir.join(".tasks/claims")).unwrap();
    std::fs::write(p.dir.join(".tasks/claims").join(&h), format!("pid {}\nexe tsk\ntime 2026-11-19T00:00:00Z\n", dead_pid())).unwrap();
    let (ok, out) = p.run_from(&p.dir, &[("PATH", path.as_str())], &["add", "blind"]);
    assert!(!ok && out.contains("nothing was written") && out.contains(&format!("remove .tasks/claims/{}", h)), "{}", out);
    assert_eq!(head(&p), h);
    // A `ps` that answers sees the writer is gone.
    let (ok, out) = p.run(&["add", "seen"]);
    assert!(ok, "{}", out);
}

// ----- 0.7: the fourth review's edges ------------------------------------

/// A third clone of the shared line's home, with its own key.
fn clone_of(m: &Machine, hub: &Proj, name: &str, key: &str) -> Proj {
    let p = Proj { dir: std::env::temp_dir().join(format!("tsk-test-{}-{}", name, std::process::id())) };
    let _ = std::fs::remove_dir_all(&p.dir);
    m.ok(&hub.dir, &["clone", "-q", ".", p.dir.to_str().unwrap()]);
    p.ok(&["key", key]);
    p
}

/// The PC lands with main ahead of origin/main and pushes its side branch
/// with the landing's merge unsettled; the Mac, whose push of that branch is
/// rejected, merges it. The branch's own upstream holds the marker, and its
/// settled base in the Mac's reflog is the Mac's own earlier push: it must
/// not decide. Refused, nothing written, naming the way out, which works.
#[test]
fn an_upstream_pushed_unsettled_is_refused_not_swapped() {
    let m = Machine::new("upstream-marker");
    let (hub, pc) = m.pair("upstream-marker", true);
    m.ok(&hub.dir, &["branch", "side/mac"]);
    let mac = clone_of(&m, &hub, "upstream-marker-lap", "q");
    m.ok(&mac.dir, &["checkout", "-q", "-b", "side/mac", "origin/side/mac"]);
    mac.ok(&["add", "mac one", "--by", "Mac: work"]);
    m.commit(&mac.dir, "mac one");
    m.ok(&mac.dir, &["push", "-q", "origin", "side/mac"]);
    pc.ok(&["add", "pc unpushed", "--by", "PC: work"]);
    m.commit(&pc.dir, "pc");
    m.ok(&pc.dir, &["fetch", "-q"]);
    m.ok(&pc.dir, &["checkout", "-q", "-b", "side/mac", "origin/side/mac"]);
    m.ok(&pc.dir, &["merge", "--no-edit", "main"]);
    assert!(pc.read(".tasks/HEAD").contains("\nmerge "), "the landing's marker");
    m.ok(&pc.dir, &["push", "-q", "origin", "side/mac"]);
    mac.ok(&["add", "mac two", "--by", "Mac: more"]);
    m.commit(&mac.dir, "mac two");
    let (ok, text) = m.git(&mac.dir, &["push", "-q", "origin", "side/mac"]);
    assert!(!ok, "the push is rejected: {}", text);
    m.ok(&mac.dir, &["fetch", "-q"]);
    let (ok, text) = m.git(&mac.dir, &["merge", "--no-edit", "origin/side/mac"]);
    assert!(!ok && text.contains("records 2 lines to merge"), "{}", text);
    m.commit(&mac.dir, "merge origin/side/mac");
    let marker = mac.read(".tasks/HEAD");
    assert!(marker.contains("merge "), "{}", marker);
    let (ok, text) = m.tsk(&mac, &["add", "more", "--by", "Mac: more"]);
    assert!(
        !ok && !text.contains("sides swapped") && text.contains("origin/side/mac") && text.contains("nothing was written") && text.contains("tsk merge --upstream"),
        "{}",
        text
    );
    // The line already merged is cleared; the refused one stays, HEAD as it was.
    let last = marker.lines().last().unwrap().to_string();
    let now = mac.read(".tasks/HEAD");
    assert!(now.starts_with(marker.lines().next().unwrap()) && now.contains(&last) && !text.contains("moved:"), "nothing written:\n{}", now);
    let (ok, text) = m.tsk(&mac, &["merge", "--upstream"]);
    assert!(ok && !text.contains("sides swapped"), "{}", text);
    let plan = mac.ok(&["plan"]);
    for t in ["mac one", "mac two", "pc unpushed"] {
        assert!(plan.contains(t), "{} is on the line:\n{}", t, plan);
    }
}

/// A cherry-picked store commit brings its event without the line before
/// it. Settling it is refused with the way out: merge the branch it came
/// from, or undo the pick. Merging the branch settles it.
#[test]
fn a_cherry_picked_event_whose_parent_is_absent_names_the_way_out() {
    let m = Machine::new("pick-orphan");
    let (_hub, pc) = m.pair("pick-orphan", true);
    m.ok(&pc.dir, &["checkout", "-q", "-b", "feat"]);
    pc.ok(&["add", "feat one", "--by", "PC: work"]);
    m.commit(&pc.dir, "feat one");
    pc.ok(&["add", "feat two", "--by", "PC: work"]);
    m.commit(&pc.dir, "feat two");
    m.ok(&pc.dir, &["checkout", "-q", "main"]);
    pc.ok(&["key", "q"]);
    pc.ok(&["add", "main's", "--by", "PC: work"]);
    pc.ok(&["key", "p"]);
    m.commit(&pc.dir, "main's");
    // Object folders are named by hash: git's directory-rename guess must not move them.
    m.ok(&pc.dir, &["-c", "merge.directoryRenames=false", "cherry-pick", "feat"]);
    let marker = pc.read(".tasks/HEAD");
    assert!(marker.contains("\nmerge "), "{}", marker);
    let (ok, text) = m.tsk(&pc, &["add", "more", "--by", "PC: more"]);
    assert!(
        !ok && text.contains("cherry-pick") && text.contains("git merge <the branch it came from>") && text.contains("git reset --keep HEAD~1") && text.contains("git cherry-pick --abort"),
        "{}",
        text
    );
    assert_eq!(pc.read(".tasks/HEAD"), marker, "nothing written");
    let (ok, text) = m.tsk(&pc, &["merge"]);
    assert!(!ok && text.contains("git reset --keep HEAD~1"), "tsk merge too:\n{}", text);
    m.ok(&pc.dir, &["-c", "merge.directoryRenames=false", "merge", "--no-edit", "feat"]);
    let (ok, text) = m.tsk(&pc, &["add", "more", "--by", "PC: more"]);
    assert!(ok, "{}", text);
    let plan = pc.ok(&["plan"]);
    for t in ["feat one", "feat two", "main's", "more"] {
        assert!(plan.contains(t), "{} is on the line:\n{}", t, plan);
    }
}

/// A fresh clone of a main pushed with its merge unsettled has no settled
/// base anywhere: naming a shared ref cannot help, so the refusal names only
/// `tsk merge --upstream`, which works.
#[test]
fn a_fresh_clone_of_an_unsettled_main_is_told_only_the_way_that_works() {
    let m = Machine::new("clone-marker");
    let (hub, _pc) = m.pair("clone-marker", true);
    m.ok(&hub.dir, &["checkout", "-q", "-b", "side"]);
    hub.ok(&["key", "s"]);
    hub.ok(&["add", "side's", "--by", "Hub: work"]);
    hub.ok(&["key", "m"]);
    m.commit(&hub.dir, "side's");
    m.ok(&hub.dir, &["checkout", "-q", "main"]);
    hub.ok(&["add", "main's", "--by", "Hub: work"]);
    m.commit(&hub.dir, "main's");
    m.ok(&hub.dir, &["merge", "--no-edit", "side"]);
    assert!(hub.read(".tasks/HEAD").contains("\nmerge "), "main committed unsettled");
    let fresh = clone_of(&m, &hub, "clone-marker-fresh", "f");
    let marker = fresh.read(".tasks/HEAD");
    let (ok, text) = m.tsk(&fresh, &["add", "more", "--by", "Fresh: work"]);
    assert!(!ok && text.contains("nothing was written") && text.contains("tsk merge --upstream") && !text.contains("tsk.shared"), "{}", text);
    assert_eq!(fresh.read(".tasks/HEAD"), marker, "nothing written");
    let (ok, text) = m.tsk(&fresh, &["merge", "--upstream"]);
    assert!(ok, "{}", text);
    let (ok, text) = m.tsk(&fresh, &["add", "more", "--by", "Fresh: work"]);
    assert!(ok, "{}", text);
}

/// A pending merge that ends as a fast-forward says so, not "replayed".
#[test]
fn a_pending_fast_forward_is_reported_as_one() {
    let (a, b) = pair("pending-ff", None);
    let ours = head(&a);
    b.ok(&["add", "bob's root"]);
    b.ok(&["add", "bob's second"]);
    bring_objects(&b.dir, &a.dir);
    let theirs = head(&b);
    std::fs::write(a.dir.join(".tasks/HEAD"), format!("{}\nmerge {}\n", ours, theirs)).unwrap();
    let (ok, text) = a.run(&["plan"]);
    assert!(ok && text.contains(&format!("fast-forward to {}: 2 event(s)", &theirs[..10])) && !text.contains("replayed"), "{}", text);
    assert_eq!(head(&a), theirs);
}

/// `tsk merge --settled`: exit 0 when .tasks/HEAD is one line with nothing
/// pending, 1 otherwise; it never writes, and never settles.
#[test]
fn the_settled_check_reads_and_never_writes() {
    let (a, b) = pair("settled-check", None);
    let (ok, text) = a.run(&["merge", "--settled"]);
    assert!(ok && text.contains("settled"), "{}", text);
    b.ok(&["add", "bob's root"]);
    bring_objects(&b.dir, &a.dir);
    let pending = format!("{}\nmerge {}\n", head(&a), head(&b));
    std::fs::write(a.dir.join(".tasks/HEAD"), &pending).unwrap();
    let (ok, text) = a.run(&["merge", "--settled"]);
    assert!(!ok && text.contains("not settled"), "{}", text);
    assert_eq!(a.read(".tasks/HEAD"), pending, "nothing written, nothing settled");
    let (ok, text) = a.run(&["--json", "merge", "--settled"]);
    assert!(!ok && text.contains("\"settled\": false"), "{}", text);
    let markers = format!("<<<<<<< HEAD\n{}\n=======\n{}\n>>>>>>> other\n", head(&a), head(&b));
    std::fs::write(a.dir.join(".tasks/HEAD"), &markers).unwrap();
    let (ok, _) = a.run(&["merge", "--settled"]);
    assert!(!ok, "conflict markers are not settled");
    assert_eq!(a.read(".tasks/HEAD"), markers);
    a.ok(&["plan"]);
    let (ok, text) = a.run(&["--json", "merge", "--settled"]);
    assert!(ok && text.contains("\"settled\": true"), "{}", text);
}
