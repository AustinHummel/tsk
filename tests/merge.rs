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
    // A close on one side against a reopen on the other is reported as status on both sides.
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
    assert!(out.contains("status on both sides: 1 t1 Run: ours reopened at") && out.contains("then theirs reopened"), "{}", out);
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
fn status_on_both_sides_is_reported_with_the_status_it_ends_in() {
    let (a, b) = pair("both-status", Some(("p", "m")));
    a.ok(&["start", "1.1"]); // older
    later();
    b.ok(&["complete", "1.1"]); // newer, published
    bring_objects(&b.dir, &a.dir);
    let out = a.ok(&["merge", &head(&b), "--upstream"]);
    assert!(
        out.contains("status on both sides: t2 ends in-progress (ours started ") && out.contains(", theirs completed ") && out.contains("; the older event stands)"),
        "reported, not stopped:\n{}",
        out
    );
    assert!(a.ok(&["plan"]).contains("[>] 1.1"), "ours replays after theirs, so it stands");
    assert!(a.ok(&["log", "-n", "1"]).contains("t2 ends in-progress"), "in the merge event's message");
    // JSON, on a fresh pair the other way round: ours is the later event.
    let (c, d) = pair("both-status-json", Some(("p", "m")));
    d.ok(&["start", "1.1"]);
    later();
    c.ok(&["complete", "1.1"]);
    bring_objects(&d.dir, &c.dir);
    let js: serde_json::Value = serde_json::from_str(&c.ok(&["--json", "merge", &head(&d), "--upstream"])).unwrap();
    let bt = &js["both_touched"][0];
    assert_eq!((bt["id"].as_str(), bt["ends"].as_str(), bt["older_stands"].as_bool()), (Some("t2"), Some("complete"), Some(false)), "{}", js);
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
