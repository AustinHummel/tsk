//! End-to-end tests against the built binary, in a temp project.

use std::path::PathBuf;
use std::process::Command;

struct Proj {
    dir: PathBuf,
}

impl Proj {
    fn new(name: &str) -> Proj {
        let dir = std::env::temp_dir().join(format!("tsk-test-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = Proj { dir };
        p.ok(&["init"]);
        p
    }
    fn run(&self, args: &[&str]) -> (bool, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_tsk")).args(args).current_dir(&self.dir).output().unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        (out.status.success(), text)
    }
    fn ok(&self, args: &[&str]) -> String {
        let (ok, text) = self.run(args);
        assert!(ok, "expected success for {:?}:\n{}", args, text);
        text
    }
    fn fails(&self, args: &[&str]) -> String {
        let (ok, text) = self.run(args);
        assert!(!ok, "expected failure for {:?}:\n{}", args, text);
        text
    }
    fn file(&self, name: &str, content: &str) -> String {
        let p = self.dir.join(name);
        std::fs::write(&p, content).unwrap();
        p.to_string_lossy().to_string()
    }
}

impl Drop for Proj {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn dana(p: &Proj) {
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
    assert!(p.ok(&["plan", "-a"]).contains("[x] 1 root"));
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
    assert!(plan.contains("1.2.1 add command (core)"), "{}", plan);
    assert!(plan.contains("1.2.4 Help entry  <tentative>"), "{}", plan);
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
    assert!(p.ok(&["plan", "-a"]).contains("[-] 1 /?"));
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
