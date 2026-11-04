//! 0.5: what the DailySpend trial asked for (the review of 2026-11-04).
//! Re-parenting with ids kept, ids in `plan`, a one-level view, colour,
//! one's own started work first, a person's requirement named as such, a
//! done-count on parents, hints on a childless task, an appendable body,
//! one separator in `render`, and the store format line.

mod common;
use common::{dana, Proj};

fn created_hash(p: &Proj, task: &str) -> String {
    let js: serde_json::Value = serde_json::from_str(&p.ok(&["--json", "show", task])).unwrap();
    let events = js["events"].as_array().unwrap();
    events.last().unwrap()["hash"].as_str().unwrap().to_string()
}

#[test]
fn adopt_moves_tasks_keeping_ids_history_links_and_numbers() {
    let p = Proj::new("adopt");
    p.ok(&["add", "Wind down", "--by", "Austin: dissolve it"]); // t1
    p.ok(&["add", "Form"]); // t2
    p.ok(&["add", "Search", "-b", "CAPTCHA-walled for agents"]); // t3
    p.ok(&["add", "Sweep"]); // t4
    p.ok(&["add", "Agent"]); // t5
    p.ok(&["set", "3", "quest=x"]);
    p.ok(&["link", "4", "3", "login first"]); // r2
    let f = p.file("bd.md", "= child: Articles\n= child: EIN\n= narrative\nform steps\n");
    p.ok(&["breakdown", "2", "-F", &f]); // t6, t7
    p.file("notes/search.md", "# Search\nShaped by [[t3]] and [the task](../tasks/t3.md).\n");
    let created = created_hash(&p, "t3");
    let before = p.ok(&["rev-list"]).lines().count();

    // A leaf parent is promoted: its own scope becomes the (core) child.
    let out = p.ok(&["adopt", "1", "3", "4", "--narrative", "both are wind-down steps"]);
    assert!(out.contains("1.1 t8 Wind down (core)") && out.contains("1.2 t3 Search  (was 3)") && out.contains("1.3 t4 Sweep  (was 4)"), "{}", out);
    assert_eq!(p.ok(&["rev-list"]).lines().count(), before + 1, "one event");
    let show = p.ok(&["show", "t3"]);
    assert!(show.starts_with("[ ] 1.2  Search"), "the new number: {}", show);
    assert!(show.contains("quest=x") && show.contains("CAPTCHA-walled") && show.contains("r2 predecessor [1.3 t4 Sweep]"), "fields, body and requirements travel: {}", show);
    assert_eq!(created_hash(&p, "t3"), created, "the same created event");
    assert!(p.ok(&["log", "t3"]).lines().next().unwrap().contains("adopted"));
    assert!(p.ok(&["backlinks", "t3"]).contains("notes/search.md:2"), "pages are named by id, so links hold");
    // The siblings left behind keep their numbers; the vacated number says where the task went.
    let plan = p.ok(&["plan"]);
    assert!(plan.contains("[ ] 2 t2 Form") && plan.contains("[ ] 5 t5 Agent  <ready>"), "{}", plan);
    let msg = p.fails(&["show", "3"]);
    assert!(msg.contains("t3 held 3 until it was adopted, and is now 1.2"), "{}", msg);
    assert!(p.fails(&["show", "3.1"]).contains("is now 1.2"));
    let narrative = p.ok(&["show", "1"]);
    assert!(narrative.contains("both are wind-down steps") && narrative.contains("1.1 t8") == false && narrative.contains("1.1 Wind down (core)"), "{}", narrative);
    // Loops reflect the move: a child depending on its parent is a loop.
    p.ok(&["link", "1", "1.2", "needs all of the parent"]);
    let loops = p.ok(&["loops"]);
    assert!(loops.contains("1.2 t3 Search"), "{}", loops);
    p.ok(&["unlink", "r3"]);
    // A broken-down parent has the narrative appended, as amend does.
    let out = p.ok(&["adopt", "1", "5", "--narrative", "the agent too"]);
    assert!(out.contains("1.4 t5 Agent  (was 5)"), "{}", out);
    let show = p.ok(&["show", "1"]);
    assert!(show.contains("both are wind-down steps") && show.contains("Adopted ") && show.contains("the agent too"), "{}", show);
    assert!(p.ok(&["plan"]).contains("1.4 t5 Agent"));
    // Refusals, each exit 1 with nothing written.
    let before = p.ok(&["rev-list"]).lines().count();
    assert!(p.fails(&["adopt", "1.2", "1", "--narrative", "x"]).contains("own ancestor"));
    assert!(p.fails(&["adopt", "1", "1", "--narrative", "x"]).contains("cannot adopt itself"));
    assert!(p.fails(&["adopt", "1", "2"]).contains("--narrative"));
    assert!(p.fails(&["adopt", "1", "1.2", "--narrative", "x"]).contains("already under"));
    assert!(p.fails(&["adopt", "2", "1.1", "--narrative", "x"]).contains("core"), "a core child stays with its host");
    p.ok(&["complete", "2.1", "2.2"]);
    assert!(p.fails(&["adopt", "2", "1.4", "--narrative", "x"]).contains("complete"));
    p.ok(&["drop", "1.4", "no agent needed"]);
    assert!(p.fails(&["adopt", "1.4", "1.3", "--narrative", "x"]).contains("dropped"));
    assert_eq!(p.ok(&["rev-list"]).lines().count(), before + 2, "only the complete and the drop were written");
    // The event, for a script. (Each adoption draws a position from the
    // task counter, so the next id is past the three adopted so far.)
    let out = p.ok(&["add", "Late"]);
    assert!(out.contains("created 6 t12 Late"), "{}", out);
    let js: serde_json::Value = serde_json::from_str(&p.ok(&["--json", "adopt", "1", "t12", "--narrative", "late too"])).unwrap();
    assert_eq!(js["kind"], "adopted");
    assert_eq!(js["subjects"], serde_json::json!(["t1", "t12"]));
    assert!(p.ok(&["diff", "t12"]).contains("+ parent: t1"));
}

#[test]
fn amend_on_a_leaf_promotes_it_like_apply() {
    let p = Proj::new("amend-leaf");
    p.ok(&["add", "Leaf", "-b", "its own scope"]);
    p.ok(&["start", "1"]);
    let out = p.ok(&["amend", "1", "Child", "--narrative", "the leaf grew a step"]);
    assert!(out.contains("added 1.2 t3 Child under 1 t1 Leaf") && out.contains("1.1 t2 Leaf (core) carries the parent's own scope"), "{}", out);
    let show = p.ok(&["show", "1"]);
    assert!(show.contains("the leaf grew a step") && !show.contains("Amended "), "a promotion's narrative is the given text: {}", show);
    assert!(p.ok(&["show", "1.1"]).contains("its own scope"));
    assert!(p.ok(&["plan"]).contains("[ ] 1 t1 Leaf"), "the parent's own status ends; the children carry it");
    // A finished leaf is not promoted.
    p.ok(&["add", "Fin"]);
    p.ok(&["complete", "2"]);
    assert!(p.fails(&["amend", "2", "More", "--narrative", "x"]).contains("complete"));
    p.ok(&["add", "Gone"]);
    p.ok(&["drop", "3", "no"]);
    assert!(p.fails(&["amend", "3", "More", "--narrative", "x"]).contains("dropped"));
}

#[test]
fn plan_prints_ids_and_takes_a_depth() {
    let p = Proj::new("plan-ids");
    dana(&p);
    let plan = p.ok(&["plan"]);
    assert!(plan.contains("[ ] 1 t1 Help Dana\n") && plan.contains("    [ ] 1.2 t3 Build a CLI\n") && plan.contains("    [ ] 1.1 t2 Gather requirements  <ready>\n"), "{}", plan);
    assert_eq!(plan.lines().filter(|l| l.contains("t3")).count(), 1, "grep by id finds the one line");
    let js: serde_json::Value = serde_json::from_str(&p.ok(&["--json", "plan"])).unwrap();
    assert_eq!(js[1]["id"], "t2");
    assert_eq!(js[1]["number"], "1.1");
    assert!(js[1].get("counts").is_none(), "a leaf has no counts");
    assert_eq!(js[0]["counts"]["open"], 3);
    let top = p.ok(&["plan", "--depth", "0"]);
    assert_eq!(top, "[ ] 1 t1 Help Dana (3 open, 1 ready)\n");
    let one = p.ok(&["plan", "--depth", "1"]);
    assert_eq!(one.lines().count(), 4);
    assert!(p.ok(&["plan", "1.2", "--depth", "0"]).starts_with("[ ] 1.2 t3 Build a CLI\n"));
}

#[test]
fn ls_is_one_level_with_leaf_counts_and_plan_marks_hidden_done_work() {
    let p = Proj::new("ls");
    p.ok(&["add", "R"]);
    let f = p.file("bd.md", "= child: a\n= child: b\n= child: c\n= narrative\nabc\n");
    p.ok(&["breakdown", "1", "-F", &f]); // t2 t3 t4
    p.ok(&["add", "S"]); // t5
    p.ok(&["complete", "1.1"]);
    p.ok(&["drop", "1.3", "no"]);
    assert_eq!(p.ok(&["ls"]), "[>] 1 t1 R (1 open, 1 ready, 1 done, 1 dropped)\n[ ] 2 t5 S  <ready>\n");
    assert_eq!(p.ok(&["ls", "1"]), "[ ] 1.2 t3 b  <ready>\n");
    assert_eq!(p.ok(&["ls", "1", "-a"]), "[x] 1.1 t2 a\n[ ] 1.2 t3 b  <ready>\n[-] 1.3 t4 c\n");
    assert!(p.ok(&["ls", "1.2"]).contains("1.2 t3 b has no children"));
    assert!(p.ok(&["ls", "--where", "nope"]).contains("(no open tasks)"));
    let js: serde_json::Value = serde_json::from_str(&p.ok(&["--json", "ls"])).unwrap();
    assert_eq!(js[0]["counts"]["done"], 1);
    assert_eq!(js[0]["counts"]["dropped"], 1);
    assert!(js[1].get("counts").is_none());
    // plan: the finished leaves it hides are counted on the parent; --all shows them and drops the marker.
    assert_eq!(p.ok(&["plan", "1"]), "[>] 1 t1 R (1 done, 1 dropped)\n    [ ] 1.2 t3 b  <ready>\n");
    assert_eq!(p.ok(&["plan", "1", "-a"]), "[>] 1 t1 R\n    [x] 1.1 t2 a\n    [ ] 1.2 t3 b  <ready>\n    [-] 1.3 t4 c\n");
    // When all children are finished, ls says so.
    p.ok(&["complete", "1.2"]);
    assert!(p.ok(&["ls", "1"]).contains("(no open children under 1 t1 R (2 done, 1 dropped); `--all` shows them)"));
}

#[test]
fn colour_is_off_without_a_terminal_and_on_when_asked() {
    let p = Proj::new("colour");
    dana(&p);
    p.ok(&["link", "1.3", "1.1", "a loop"]);
    assert!(!p.ok(&["plan"]).contains('\x1b'), "piped output is plain");
    assert!(!p.ok(&["show", "1.1"]).contains('\x1b'));
    let on = p.ok(&["--color", "always", "plan"]);
    assert!(on.contains("\x1b[2mt1\x1b[0m"), "ids are dim: {:?}", on);
    assert!(on.contains("\x1b[31mloop\x1b[0m"), "loop tags are red: {:?}", on);
    assert!(!p.ok(&["--color", "always", "--json", "plan"]).contains('\x1b'), "never in JSON");
    assert!(p.ok(&["--color", "always", "show", "1.1"]).contains("\x1b[1mrequired by:\x1b[0m"), "headers are bold");
    assert!(p.ok(&["--color", "always", "log", "-n", "1"]).contains("\x1b[2m"), "hashes are dim");
    let (ok, out) = p.run_from(&p.dir, &[], &["--color", "sometimes", "plan"]);
    assert!(!ok && out.contains("always"), "{}", out);
    // Config and the environment, in git's spirit.
    p.ok(&["config", "color.ui", "always"]);
    assert!(p.ok(&["plan"]).contains('\x1b'));
    assert!(!p.ok(&["--color", "never", "plan"]).contains('\x1b'));
    let (_, out) = p.run_from(&p.dir, &[("NO_COLOR", "1")], &["plan"]);
    assert!(!out.contains('\x1b'), "NO_COLOR wins over config");
    p.ok(&["config", "color.ui", "false"]);
    assert!(!p.ok(&["plan"]).contains('\x1b'));
    assert!(p.ok(&["--color", "always", "plan"]).contains('\x1b'), "the flag wins over config");
}

#[test]
fn ready_opens_with_the_authors_own_in_progress_tasks() {
    let p = Proj::new("ready-you");
    p.ok(&["add", "a"]);
    p.ok(&["add", "b"]);
    p.ok(&["add", "c"]);
    p.ok(&["set", "2", "agent=bob"]);
    p.run_from(&p.dir, &[("TSK_AUTHOR", "alice")], &["start", "1"]);
    p.run_from(&p.dir, &[("TSK_AUTHOR", "bob")], &["start", "2"]);
    let (_, alice) = p.run_from(&p.dir, &[("TSK_AUTHOR", "alice")], &["ready"]);
    assert!(alice.starts_with("in progress (you):\n  [>] 1 t1 a\nready:\n  [ ] 3 t3 c\n"), "{}", alice);
    let (_, bob) = p.run_from(&p.dir, &[("TSK_AUTHOR", "bob")], &["ready"]);
    assert!(bob.starts_with("in progress (you):\n  [>] 2 t2 b\n") && !bob.contains("t1 a"), "{}", bob);
    let (_, carol) = p.run_from(&p.dir, &[("TSK_AUTHOR", "carol")], &["ready"]);
    assert!(carol.starts_with("ready:\n"), "nothing of hers: {}", carol);
    let (_, bob) = p.run_from(&p.dir, &[("TSK_AUTHOR", "bob")], &["ready", "--where", "agent=bob"]);
    assert!(bob.contains("in progress (you):\n  [>] 2 t2 b\n"), "filters apply: {}", bob);
    let (_, alice) = p.run_from(&p.dir, &[("TSK_AUTHOR", "alice")], &["ready", "--where", "agent=bob"]);
    assert!(!alice.contains("in progress"), "{}", alice);
    // The store's author config, then the OS user, stand in for TSK_AUTHOR.
    p.ok(&["config", "author", "alice"]);
    assert!(p.ok(&["ready"]).contains("in progress (you):\n  [>] 1 t1 a\n"));
    let (_, js) = p.run_from(&p.dir, &[("TSK_AUTHOR", "bob")], &["--json", "ready"]);
    let js: serde_json::Value = serde_json::from_str(&js).unwrap();
    assert_eq!(js["author"], "bob");
    assert_eq!(js["in_progress"][0]["author"], "alice");
    assert_eq!(js["in_progress"][0]["mine"], false);
    assert_eq!(js["in_progress"][1]["mine"], true);
}

#[test]
fn a_persons_requirement_is_labelled_required_by() {
    let p = Proj::new("by-label");
    p.ok(&["add", "x", "--by", "Dana: why"]);
    p.ok(&["add", "y"]);
    p.ok(&["require", "2", "1", "an interface"]);
    let show = p.ok(&["show", "1"]);
    assert!(show.contains("required by:\n  r1 by [Dana]: why\n  r2 technical [2 t2 y]: an interface"), "{}", show);
    let first = p.ok(&["rev-list"]).lines().last().unwrap().to_string();
    let diff = p.ok(&["diff", "--from", &first]);
    assert!(diff.contains("+ r1 required by [Dana] on [1 t1 x]: why") && diff.contains("+ r2 technical on [1 t1 x] from [2 t2 y]: an interface"), "{}", diff);
    let t = p.ok(&["breakdown", "1", "--template"]);
    assert!(t.contains("# r1: required by [Dana] on [1 t1 x]: why") && t.contains("# r2: technical on"), "{}", t);
    assert!(!show.contains("technical [Dana]") && !t.contains("technical on [1 t1 x] from [Dana]"));
    assert!(p.ok(&["context", "1"]).contains("required by Dana: why"));
}

#[test]
fn plan_on_a_childless_task_says_what_attaches_to_it() {
    let p = Proj::new("hints");
    p.ok(&["add", "Quest", "--by", "Austin: the quest"]);
    p.ok(&["set", "1", "quest=q"]);
    p.ok(&["add", "Step one"]);
    p.ok(&["set", "2", "quest=q"]);
    p.ok(&["add", "Step two"]);
    p.ok(&["set", "3", "quest=q"]);
    p.ok(&["add", "Done step"]);
    p.ok(&["set", "4", "quest=q"]);
    p.ok(&["complete", "4"]);
    p.ok(&["link", "1", "2", "the quest first"]);
    let out = p.ok(&["plan", "1"]);
    assert_eq!(out, "[ ] 1 t1 Quest  <ready>\n    no children\n    2 open tasks carry quest=q: tsk plan --where quest=q\n    2 requirements on it: tsk show 1\n", "{}", out);
    let out = p.ok(&["plan", "3"]);
    assert!(out.contains("2 open tasks carry quest=q") && !out.contains("requirement"), "{}", out);
    assert_eq!(p.ok(&["plan", "2"]).lines().count(), 4, "the line, no children, its field, and the requirement it places");
    assert!(p.ok(&["plan", "2"]).contains("1 it places: tsk show 2"));
    p.ok(&["add", "Plain"]);
    assert_eq!(p.ok(&["plan", "5"]), "[ ] 5 t5 Plain  <ready>\n", "nothing attaches: one line");
    let js: serde_json::Value = serde_json::from_str(&p.ok(&["--json", "plan", "1"])).unwrap();
    assert_eq!(js.as_array().unwrap().len(), 1, "JSON carries no hint lines");
    let whole = p.ok(&["plan", "--where", "quest=q"]);
    assert!(!whole.contains("no children"), "the hint is for one named task: {}", whole);
}

#[test]
fn edit_append_adds_dated_paragraphs_without_replacing() {
    let p = Proj::new("append");
    p.ok(&["add", "n", "-b", "the brief"]);
    let out = p.ok(&["edit", "1", "--append", "first result"]);
    assert!(out.contains("appended to body"), "{}", out);
    p.ok(&["edit", "1", "--append", "second result"]);
    let show = p.ok(&["show", "1"]);
    let (a, b, c) = (show.find("the brief").unwrap(), show.find(": first result").unwrap(), show.find(": second result").unwrap());
    assert!(a < b && b < c, "in order: {}", show);
    let date = &show[b - 10..b];
    assert!(date.starts_with("20") && date.chars().filter(|c| *c == '-').count() == 2, "dated: {}", date);
    assert_eq!(p.ok(&["log", "1"]).lines().filter(|l| l.contains("edited")).count(), 2, "one event each");
    assert!(p.fails(&["edit", "1", "--append", "x", "-b", "y"]).contains("cannot be combined"));
    assert!(p.fails(&["edit", "1", "--append", "  "]).contains("nothing to append"));
    let js: serde_json::Value = serde_json::from_str(&p.ok(&["--json", "edit", "1", "--append", "third"])).unwrap();
    assert_eq!(js["kind"], "edited");
    p.ok(&["add", "empty"]);
    p.ok(&["edit", "2", "--append", "only note"]);
    assert!(p.ok(&["show", "2"]).contains(": only note"));
}

#[test]
fn render_reports_its_folder_with_one_separator() {
    let p = Proj::new("render-sep");
    p.ok(&["add", "root"]);
    let store = p.dir.to_string_lossy().replace('\\', "/");
    let (ok, out) = p.run_from(&std::env::temp_dir(), &[], &["--store", &store, "render"]);
    assert!(ok && out.contains("/tasks (") && !out.contains('\\'), "{}", out);
    let (_, js) = p.run_from(&std::env::temp_dir(), &[], &["--store", &store, "--json", "render"]);
    assert!(!js.contains("\\\\"), "{}", js);
}

#[test]
fn the_store_declares_its_format_and_a_newer_one_is_refused() {
    let p = Proj::new("format");
    assert!(p.read(".tasks/config").contains("format = 1"), "{}", p.read(".tasks/config"));
    // A store from before 0.5.0 has no line: it is read, and the first write adds one.
    std::fs::write(p.dir.join(".tasks/config"), "# tsk config: key = value\nauthor = old\n").unwrap();
    assert!(p.ok(&["plan"]).contains("no open tasks"));
    assert!(!p.read(".tasks/config").contains("format"), "a read declares nothing");
    p.ok(&["add", "root"]);
    let cfg = p.read(".tasks/config");
    assert!(cfg.contains("format = 1") && cfg.contains("author = old"), "{}", cfg);
    // A store written by a newer tsk is refused, read or write, with one sentence.
    p.ok(&["config", "format", "9"]);
    let msg = p.fails(&["plan"]);
    assert!(msg.contains("format 9") && msg.contains("upgrade tsk") && msg.lines().count() == 1, "{}", msg);
    assert!(p.fails(&["add", "x"]).contains("upgrade tsk"));
    assert!(!p.read(".tasks/log").contains("add x"));
}
