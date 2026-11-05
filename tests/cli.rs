//! End-to-end tests against the built binary, in a temp project.

mod common;
use common::{dana, Proj};

#[test]
fn breakdown_is_atomic_and_hands_requirements_down() {
    let p = Proj::new("breakdown");
    dana(&p);
    // Missing a disposition: refused, nothing changes.
    let bad = p.file("bad.md", "= child: a\n= child: b\n= narrative\nx\n= req r1 -> 1\n");
    let msg = p.fails(&["breakdown", "1.2", "-F", &bad]);
    assert!(msg.contains("missing"), "{}", msg);
    assert!(!p.ok(&["plan"]).contains("1.2.1"));
    // No narrative: refused.
    let bad2 = p.file("bad2.md", "= child: a\n= req r1 -> 1\n= req r2 -> 1\n= req r3 -> all\n");
    assert!(p.fails(&["breakdown", "1.2", "-F", &bad2]).contains("narrative"));
    // Good: r2 (fair) to child 2, r3 (help) to child 3, r4 stays (r1 is Dana's).
    let good = p.file("good.md", "= child: store\n= child: pick\n= child: help\n= narrative\nstore, pick, help\n= req r2 -> 2\n= req r3 -> 3\n= req r4 -> all\n");
    p.ok(&["breakdown", "1.2", "-F", &good]);
    let ready = p.ok(&["ready"]);
    assert!(ready.contains("1.2.1"), "store is unblocked: {}", ready);
    assert!(!ready.contains("1.2.2"), "pick still waits on 1.1: {}", ready);
    let show = p.ok(&["show", "1.2.2"]);
    assert!(show.contains("r2") && show.contains("what fair means"), "{}", show);
    // 1.3 inherits r4 from the parent level: it waits on everything under 1.2.
    let blocked = p.ok(&["ready", "--blocked"]);
    assert!(blocked.contains("1.3") && blocked.contains("r4"), "{}", blocked);
}

#[test]
fn loops_are_found_and_guarded() {
    let p = Proj::new("loops");
    p.ok(&["add", "root"]);
    let f = p.file("bd.md", "= child: a\n= child: b\n= child: c\n= narrative\nabc\n");
    p.ok(&["breakdown", "1", "-F", &f]);
    p.ok(&["link", "1.1", "1.2", "a first"]);
    p.ok(&["link", "1.2", "1.3", "b first"]);
    let out = p.ok(&["link", "1.3", "1.1", "c first"]);
    assert!(out.contains("loop"), "{}", out);
    let loops = p.ok(&["loops"]);
    assert!(loops.contains("loop #1") && loops.contains("r3"), "{}", loops);
    assert!(p.fails(&["start", "1.1"]).contains("in a loop"));
    assert!(p.fails(&["complete", "1.1", "1.2"]).contains("in a loop"));
    // The whole group at once is allowed, and recorded as a group.
    let out = p.ok(&["complete", "1.1", "1.2", "1.3"]);
    assert!(!out.contains("FORCED"), "{}", out);
    assert!(p.ok(&["forced"]).contains("no forced events"));
    assert!(p.ok(&["plan", "-a"]).contains("[x] 1 t1 root"));
}

#[test]
fn child_depending_on_parent_is_a_loop() {
    let p = Proj::new("parentloop");
    p.ok(&["add", "root"]);
    let f = p.file("bd.md", "= child: a\n= child: b\n= narrative\nab\n");
    p.ok(&["breakdown", "1", "-F", &f]);
    p.ok(&["link", "1", "1.2", "b needs all of root"]);
    let loops = p.ok(&["loops"]);
    assert!(loops.contains("1.2 "), "{}", loops);
    assert!(!loops.contains("1.1 "), "a is not in the loop: {}", loops);
}

#[test]
fn force_is_recorded() {
    let p = Proj::new("force");
    p.ok(&["add", "root"]);
    let f = p.file("bd.md", "= child: a\n= child: b\n= narrative\nab\n");
    p.ok(&["breakdown", "1", "-F", &f]);
    p.ok(&["link", "1.1", "1.2", "a first"]);
    p.fails(&["complete", "1.2"]);
    let out = p.ok(&["complete", "1.2", "--force"]);
    assert!(out.contains("FORCED"), "{}", out);
    assert!(p.ok(&["forced"]).contains("complete 1.2"));
    // Force is not recorded when nothing was overridden.
    let out = p.ok(&["complete", "1.1", "--force"]);
    assert!(!out.contains("FORCED"), "{}", out);
}

#[test]
fn types_apply_as_a_breakdown_and_track_instances() {
    let p = Proj::new("types");
    p.ok(&["add", "root"]);
    let f = p.file("bd.md", "= child: readme\n= child: add command\n= child: ask\n= narrative\nx\n");
    p.ok(&["breakdown", "1", "-F", &f]);
    p.ok(&["type", "add", "cmd", "Command"]);
    p.ok(&["type", "item", "cmd", "cmd-2", "Readme entry", "--req", "1.1: readme first"]);
    p.ok(&["type", "item", "cmd", "cmd-3", "Tests", "--req", "core: command first"]);
    p.ok(&["type", "item", "cmd", "cmd-1", "Help entry", "--pending", "1.3"]);
    p.ok(&["apply", "cmd", "1.2"]);
    let plan = p.ok(&["plan"]);
    assert!(plan.contains("1.2.1 t5 add command (core)"), "{}", plan);
    assert!(plan.contains("1.2.4 t8 Help entry  <tentative>"), "{}", plan);
    assert!(p.ok(&["ready"]).contains("1.2.1"));
    assert!(p.fails(&["start", "1.2.3"]).contains("waits on 1.2.1"));
    assert!(p.fails(&["apply", "cmd", "1.2"]).contains("already broken down"));
    // Editing the item lists the instance.
    let out = p.ok(&["type", "edit-item", "cmd", "cmd-3", "--body", "also integration tests"]);
    assert!(out.contains("1.2.3"), "{}", out);
    // Tentative stays tentative after the condition completes, until confirmed.
    p.ok(&["complete", "1.3"]);
    assert!(p.fails(&["start", "1.2.4"]).contains("confirm"));
    p.ok(&["confirm", "1.2.4"]);
    p.ok(&["start", "1.2.4"]);
}

#[test]
fn history_is_immutable_and_diffable() {
    let p = Proj::new("history");
    p.ok(&["add", "root", "-b", "first"]);
    p.ok(&["edit", "1", "-b", "second"]);
    let diff = p.ok(&["diff", "1"]);
    assert!(diff.contains("- first") && diff.contains("+ second"), "{}", diff);
    let log = p.ok(&["log"]);
    assert!(log.contains("edited") && log.contains("created") && log.contains("init"), "{}", log);
    let revs = p.ok(&["rev-list"]);
    assert_eq!(revs.lines().count(), 3);
    let head = revs.lines().next().unwrap().to_string();
    assert!(p.ok(&["cat-object", &head]).starts_with("event"));
    assert!(p.fails(&["uncomplete", "1"]).contains("not complete"));
}

#[test]
fn tasks_resolve_by_id_number_or_title() {
    let p = Proj::new("resolve");
    p.ok(&["add", "/?"]);
    p.ok(&["add", "Build a CLI"]);
    assert!(p.ok(&["show", "build a cli"]).contains("t2"));
    assert!(p.ok(&["show", "2"]).contains("Build a CLI"));
    let msg = p.fails(&["show", "cli"]);
    assert!(msg.contains("did you mean") && msg.contains("Build a CLI"), "{}", msg);
    p.ok(&["drop", "/?", "created by accident"]);
    assert!(p.ok(&["plan"]).contains("(no open tasks)") == false);
    assert!(p.ok(&["plan", "-a"]).contains("[-] 1 t1 /?"));
}

#[test]
fn agents_get_a_non_interactive_path() {
    let p = Proj::new("agent");
    p.ok(&["add", "root", "--by", "Dana: wants it"]);
    // No terminal in tests: the editor path fails fast instead of hanging.
    let msg = p.fails(&["breakdown", "1"]);
    assert!(msg.contains("-F"), "{}", msg);
    assert!(p.fails(&["edit", "1"]).contains("-F"));
    // The template names the person's requirement as optional.
    let t = p.ok(&["breakdown", "1", "--template"]);
    assert!(t.contains("# = req r1 ->"), "{}", t);
    // A person's requirement may be omitted, or sent to a child.
    let f = p.file("bd.md", "= child: a\n= child: b\n= narrative\nab\n= req r1 -> 2\n");
    let out = p.ok(&["--json", "breakdown", "1", "-F", &f]);
    assert!(out.contains("\"kind\": \"broken-down\"") && out.contains("\"t3\""), "{}", out);
    assert!(p.ok(&["show", "1.2"]).contains("Dana"));
    assert!(!p.ok(&["show", "1"]).contains("Dana"));
    // A change in --json mode prints only the event.
    let out = p.ok(&["--json", "complete", "1.1"]);
    assert!(out.trim_start().starts_with('{') && out.contains("\"completed\""), "{}", out);
    assert!(!out.contains("now ready"), "{}", out);
}

// ----- 0.6: ongoing scopes and dates -----------------------------------------

#[test]
fn an_ongoing_task_stays_open_until_closed_and_is_never_ready() {
    let p = Proj::new("ongoing");
    let out = p.ok(&["add", "Run the business", "--by", "A: it exists", "--ongoing", "What the business owes while it exists."]);
    assert!(out.contains("<ongoing>"), "{}", out);
    assert!(p.ok(&["plan"]).contains("[ ] 1 t1 Run the business  <ongoing>"));
    assert!(!p.ok(&["ready"]).contains("t1"), "never ready, even childless");
    // Pieces are added with their fit; no placeholder child appears.
    let out = p.ok(&["amend", "1", "Register", "--fit", "the register"]);
    assert!(out.contains("1 t1 Run the business admits: What the business owes while it exists."), "{}", out);
    p.ok(&["amend", "1", "Taxes", "--fit", "the yearly return"]);
    let plan = p.ok(&["plan"]);
    assert!(!plan.contains("(core)") && plan.contains("1.1 t2 Register") && plan.contains("1.2 t3 Taxes"), "{}", plan);
    assert!(!p.ok(&["ready"]).contains("t1 Run"), "{}", p.ok(&["ready"]));
    p.ok(&["complete", "1.1"]);
    p.ok(&["complete", "1.2"]);
    let plan = p.ok(&["plan"]);
    assert!(plan.contains("[>] 1 t1 Run the business") && !plan.contains("[x] 1 t1"), "running, nothing open under it: {}", plan);
    // Closing needs nothing open under it.
    p.ok(&["uncomplete", "1.1"]);
    let msg = p.fails(&["close", "1", "dissolved"]);
    assert!(msg.contains("1 open under 1 t1 Run the business") && msg.contains("1.1 t2 Register"), "{}", msg);
    p.ok(&["complete", "1.1"]);
    let out = p.ok(&["close", "1", "dissolved 2030-01-01"]);
    assert!(out.contains("closed 1 t1 Run the business: dissolved 2030-01-01"), "{}", out);
    assert!(p.ok(&["plan", "-a"]).contains("[x] 1 t1 Run the business"));
    assert!(!p.ok(&["plan"]).contains("t1"));
    assert!(p.ok(&["show", "1"]).contains("ongoing, closed"));
    assert!(p.fails(&["close", "1", "again"]).contains("already closed"));
    assert!(p.fails(&["amend", "1", "More", "--fit", "m"]).contains("is closed; tsk reopen it first"));
    p.ok(&["reopen", "1"]);
    assert!(p.ok(&["plan"]).contains("[>] 1 t1 Run the business"));
    assert!(p.fails(&["reopen", "1"]).contains("is not closed"));
    // It has no end of its own.
    for args in [vec!["uncomplete", "1"], vec!["start", "1"], vec!["complete", "1"], vec!["pause", "1"], vec!["spike", "1"]] {
        let msg = p.fails(&args);
        assert!(msg.contains("is ongoing"), "{:?}: {}", args, msg);
    }
    p.ok(&["type", "add", "x", "X"]);
    p.ok(&["type", "item", "x", "i", "Item"]);
    assert!(p.fails(&["apply", "x", "1"]).contains("apply it to a child of 1"));
    // A finite task is declared ongoing by hand, and taken back.
    p.ok(&["add", "Duty"]);
    assert!(p.fails(&["close", "2", "x"]).contains("is not ongoing"));
    p.ok(&["ongoing", "2", "Every account's renewal."]);
    assert!(p.ok(&["plan"]).contains("[ ] 2 t4 Duty  <ongoing>"));
    assert!(p.ok(&["show", "2"]).contains("admits:\n  Every account's renewal."));
    p.ok(&["ongoing", "2", "--off"]);
    assert!(p.ok(&["plan"]).contains("[ ] 2 t4 Duty  <ready>"));
    let js: serde_json::Value = serde_json::from_str(&p.ok(&["--json", "ongoing", "2", "back on"])).unwrap();
    assert_eq!(js["kind"], "set");
}

#[test]
fn ongoing_is_refused_as_a_predecessor_and_coarse_links_are_noted() {
    let p = Proj::new("ongoing-links");
    p.ok(&["add", "Run", "--ongoing", "runs"]); // t1
    p.ok(&["add", "X"]); // t2
    let msg = p.fails(&["link", "1", "2", "x waits on the business"]);
    assert!(msg.contains("1 t1 Run never completes on its own (ongoing); link the piece of it you wait on"), "{}", msg);
    // A parent whose subtree holds an ongoing task is refused too, naming it.
    p.ok(&["add", "P"]); // t3
    let f = p.file("bd.md", "= child: a\n= child: b\n= narrative\nab\n");
    p.ok(&["breakdown", "3", "-F", &f]); // t4 t5
    p.ok(&["amend", "3", "Duty", "--fit", "the duty"]); // t6
    p.ok(&["ongoing", "3.3", "what the duty admits"]);
    let msg = p.fails(&["link", "3", "2", "x waits on all of P"]);
    assert!(msg.contains("(ongoing: 3.3)"), "{}", msg);
    // A predecessor cannot become ongoing.
    p.ok(&["add", "Q"]); // t7
    p.ok(&["link", "4", "2", "q first"]);
    let msg = p.fails(&["ongoing", "4", "q admits"]);
    assert!(msg.contains("4 t7 Q is a predecessor of 2 t2 X (r") && msg.contains("an ongoing task never completes on its own"), "{}", msg);
    // Nor can one be adopted under a predecessor.
    let msg = p.fails(&["adopt", "4", "1", "--fit", "the business", "--core", "Q itself"]);
    assert!(msg.contains("an ongoing task under it would make them wait until it is closed"), "{}", msg);
    // A link onto a parent waits on everything under it: said, not refused.
    p.ok(&["add", "R"]); // t8
    let f = p.file("bd3.md", "= child: r1\n= child: r2\n= child: r3\n= narrative\nrrr\n");
    p.ok(&["breakdown", "5", "-F", &f]);
    let out = p.ok(&["link", "5", "2", "x waits on all of R"]);
    assert!(out.contains("note: 5 t8 R has 3 open under it; the link waits on all of them"), "{}", out);
}

#[test]
fn dates_gate_readiness_and_are_shown() {
    let p = Proj::new("dates");
    p.ok(&["add", "root"]);
    let f = p.file("bd.md", "= child: a\n= child: b\n= child: c\n= narrative\nabc\n");
    p.ok(&["breakdown", "1", "-F", &f]); // t2 t3 t4
    let at = |now: &str, args: &[&str]| p.run_from(&p.dir, &[("TSK_NOW", now)], args);
    let out = p.ok(&["when", "1.2", "--after", "2027-03-16", "--due", "2027-04-09"]);
    assert!(out.contains("when 1.2 t3 b: after=2027-03-16 due=2027-04-09"), "{}", out);
    assert!(p.ok(&["when", "1.2", "--after", "2027-03-16"]).contains("nothing changed"));
    let (_, ready) = at("2027-03-01", &["ready"]);
    assert_eq!(ready, "ready:\n  [ ] 1.1 t2 a\n  [ ] 1.3 t4 c\nnot before:\n  [ ] 1.2 t3 b  not before 2027-03-16, due 2027-04-09\n", "{}", ready);
    let (ok, msg) = at("2027-03-01", &["start", "1.2"]);
    assert!(!ok && msg.contains("1.2 t3 b: not before 2027-03-16"), "{}", msg);
    // An early start is forced, and recorded.
    p.ok(&["when", "1.3", "--after", "2027-03-16"]);
    let (ok, out) = at("2027-03-01", &["start", "1.3", "--force"]);
    assert!(ok && out.contains("FORCED"), "{}", out);
    assert!(p.ok(&["forced"]).contains("start 1.3"));
    let (_, ready) = at("2027-03-16", &["ready"]);
    assert!(ready.contains("ready:\n  [ ] 1.1 t2 a\n  [ ] 1.2 t3 b  due 2027-04-09\n") && !ready.contains("not before"), "the day itself: {}", ready);
    let (_, plan) = at("2027-05-01", &["plan"]);
    assert!(plan.contains("1.2 t3 b  <ready, overdue>"), "{}", plan);
    assert!(at("2027-05-01", &["ready"]).1.contains("due 2027-04-09 (overdue)"));
    // A date on a parent gates every leaf under it.
    p.ok(&["when", "1", "--after", "2027-06-01", "nothing before the summer"]);
    let (_, ready) = at("2027-05-01", &["ready"]);
    assert!(ready.contains("not before:\n  [ ] 1.1 t2 a  not before 2027-06-01 (after, on 1)"), "{}", ready);
    assert!(p.ok(&["show", "1.1"]).contains("not before 2027-06-01 (after, on 1)"));
    p.ok(&["when", "1", "--clear", "after"]);
    assert!(at("2027-05-01", &["ready"]).1.contains("ready:\n  [ ] 1.1 t2 a\n"));
    // add --after, and a bad date.
    p.ok(&["add", "later", "--after", "2027-01-01"]);
    assert!(at("2026-12-31", &["plan"]).1.contains("2 t5 later  <not before 2027-01-01>"));
    assert!(at("2027-01-01", &["plan"]).1.contains("2 t5 later  <ready>"));
    let msg = p.fails(&["when", "1.2", "--after", "notadate"]);
    assert!(msg.contains("YYYY-MM-DD"), "{}", msg);
    assert!(p.fails(&["when", "1.2", "--clear", "both"]).contains("--clear takes"));
    // The lapse case: a wait on an outside party until a deadline.
    p.ok(&["add", "File the articles"]); // t6
    p.ok(&["start", "3"]);
    p.ok(&["pause", "3"]);
    p.ok(&["when", "3", "--after", "2026-11-10", "a reply, or Tuesday without it"]);
    let (_, ready) = at("2026-11-06", &["ready"]);
    assert!(ready.contains("not before:\n  [=] 3 t6 File the articles  not before 2026-11-10"), "{}", ready);
    assert!(!at("2026-11-06", &["start", "3"]).0);
    // The evening before, the date still holds: a date is the start of its local day.
    assert!(at("2026-11-09T20:00", &["ready"]).1.contains("not before 2026-11-10"));
    // Without the clear, the first ready on the day lists it.
    let (_, ready) = at("2026-11-10", &["ready"]);
    assert!(ready.contains("\n  [=] 3 t6 File the articles\n") && !ready.contains("not before 2026-11-10"), "{}", ready);
    // The reply came first: cleared by hand, ready that day.
    let (ok, _) = at("2026-11-07", &["when", "3", "--clear", "after", "replied 2026-11-07"]);
    assert!(ok);
    let (_, ready) = at("2026-11-07", &["ready"]);
    assert!(ready.contains("\n  [=] 3 t6 File the articles\n") && !ready.contains("not before 2026-11-10"), "{}", ready);
    let log = p.ok(&["log", "3"]);
    assert!(log.contains("when 3 after=2026-11-10: a reply, or Tuesday without it") && log.contains("when 3 -after: replied 2026-11-07"), "{}", log);
    // A time is local when entered and stored with its offset; it gates to the minute.
    p.ok(&["when", "3", "--after", "2026-11-10T09:00"]);
    let js: serde_json::Value = serde_json::from_str(&p.ok(&["--json", "show", "3"])).unwrap();
    let stored = js["task"]["after"].as_str().unwrap();
    assert!(stored.starts_with("2026-11-10T09:00") && stored.len() == 22, "{}", stored);
    assert!(at("2026-11-10T08:59", &["ready"]).1.contains("not before 2026-11-10T09:00"));
    let (_, ready) = at("2026-11-10T09:00", &["ready"]);
    assert!(ready.contains("\n  [=] 3 t6 File the articles\n") && !ready.contains("not before 2026-11-10"), "{}", ready);
    assert!(!at("bogus", &["ready"]).0);
    // Determinism: statuses never depend on the date; only readiness does.
    let statuses = |now: &str| -> Vec<String> {
        let js: serde_json::Value = serde_json::from_str(&at(now, &["--json", "plan", "-a"]).1).unwrap();
        js.as_array().unwrap().iter().map(|r| format!("{} {}", r["id"], r["status"])).collect()
    };
    assert_eq!(statuses("2026-01-01"), statuses("2028-01-01"));
}

#[test]
fn completing_the_last_piece_under_an_ongoing_task_nudges_the_next() {
    let p = Proj::new("nudge");
    p.ok(&["add", "Duty", "--ongoing", "one dated check at a time"]); // t1
    p.ok(&["amend", "1", "Check 2027", "--fit", "the 2027 check"]); // t2
    p.ok(&["add", "Standing", "--ongoing", "never filled yet"]); // t3
    let out = p.ok(&["complete", "1.1"]);
    assert!(out.contains("1 t1 Duty is ongoing and nothing is open under it now: add the next piece (tsk amend 1 ... --after ...), or tsk close it"), "{}", out);
    let ready = p.ok(&["ready"]);
    assert!(ready.contains("ongoing, nothing open under it: 1 t1 Duty") && !ready.contains("t3"), "{}", ready);
    let js: serde_json::Value = serde_json::from_str(&p.ok(&["--json", "ready"])).unwrap();
    assert_eq!(js["empty_ongoing"], serde_json::json!(["t1"]));
    p.ok(&["amend", "1", "Check 2028", "--fit", "the 2028 check", "--after", "2028-03-01"]);
    assert!(!p.ok(&["ready"]).contains("nothing open under it"));
}
