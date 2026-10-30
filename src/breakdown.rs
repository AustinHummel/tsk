//! The breakdown file: what `tsk breakdown <task>` reads, from `-F` or from
//! the editor. A small directive format, readable as markdown:
//!
//! ```text
//! = child: Decide on tech stack
//! Freeform body for this child, until the next directive.
//!
//! = child: Create repo
//!
//! = narrative
//! How the children together fulfill the parent.
//!
//! = req r3 -> 2
//! = req r4 -> all
//! ```
//!
//! Every requirement on the parent, or placed by it, must be dispositioned:
//! to a child by number, or `all` to keep it on the parent.

use anyhow::{bail, Result};
use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub struct BreakdownFile {
    pub children: Vec<(String, String)>,
    pub narrative: String,
    /// req id -> child index (1-based) or 0 for `all`.
    pub dispositions: BTreeMap<String, usize>,
}

pub fn parse(text: &str) -> Result<BreakdownFile> {
    let mut out = BreakdownFile::default();
    enum Sect {
        None,
        Child(usize),
        Narrative,
    }
    let mut sect = Sect::None;
    let mut bodies: Vec<String> = Vec::new();
    let mut narrative = String::new();
    for (ln, raw) in text.lines().enumerate() {
        let line = raw.trim_end();
        if let Some(rest) = line.strip_prefix("= ") {
            let rest = rest.trim();
            if let Some(title) = rest.strip_prefix("child:") {
                let title = title.trim();
                if title.is_empty() {
                    bail!("line {}: child has no title", ln + 1);
                }
                out.children.push((title.to_string(), String::new()));
                bodies.push(String::new());
                sect = Sect::Child(out.children.len() - 1);
            } else if rest == "narrative" {
                sect = Sect::Narrative;
            } else if let Some(d) = rest.strip_prefix("req ") {
                let (id, target) = d
                    .split_once("->")
                    .ok_or_else(|| anyhow::anyhow!("line {}: expected `= req <id> -> <child#|all>`", ln + 1))?;
                let id = id.trim().to_string();
                let target = target.trim();
                let idx = if target == "all" {
                    0
                } else {
                    target
                        .parse::<usize>()
                        .map_err(|_| anyhow::anyhow!("line {}: `{}` is not a child number or `all`", ln + 1, target))?
                };
                out.dispositions.insert(id, idx);
                sect = Sect::None;
            } else {
                bail!("line {}: unknown directive `{}`", ln + 1, rest);
            }
            continue;
        }
        if line.starts_with('#') && matches!(sect, Sect::None) {
            continue; // comments before the first directive
        }
        match sect {
            Sect::None => {
                if !line.trim().is_empty() {
                    bail!("line {}: text outside any section", ln + 1);
                }
            }
            Sect::Child(i) => {
                bodies[i].push_str(line);
                bodies[i].push('\n');
            }
            Sect::Narrative => {
                narrative.push_str(line);
                narrative.push('\n');
            }
        }
    }
    for (i, b) in bodies.into_iter().enumerate() {
        out.children[i].1 = b.trim().to_string();
    }
    out.narrative = narrative.trim().to_string();
    if out.children.is_empty() {
        bail!("a breakdown needs at least one `= child:` line");
    }
    for (id, idx) in &out.dispositions {
        if *idx > out.children.len() {
            bail!("requirement {} is sent to child {}, but there are only {}", id, idx, out.children.len());
        }
    }
    Ok(out)
}

/// The template the editor opens with: the parent's card for reference, then
/// a child stub and one `= req` line per requirement to disposition.
pub fn template(parent_label: &str, parent_body: &str, reqs: &[(String, String)], optional: &[(String, String)]) -> String {
    let mut s = String::new();
    s.push_str(&format!("# Breakdown of {}\n", parent_label));
    s.push_str("# Lines starting with `#` before the first directive are ignored.\n");
    s.push_str("# One `= child: <title>` per child, its scope below it. Then `= narrative`.\n");
    s.push_str("# Every requirement listed below must go to a child number or `all`.\n");
    if !parent_body.trim().is_empty() {
        s.push_str("#\n# Parent card:\n");
        for l in parent_body.lines() {
            s.push_str(&format!("#   {}\n", l));
        }
    }
    s.push_str("\n= child: \n\n= child: \n\n= narrative\n\n");
    for (id, desc) in reqs {
        s.push_str(&format!("# {}: {}\n= req {} -> \n", id, desc, id));
    }
    for (id, desc) in optional {
        s.push_str(&format!("# {}: {}\n# (a person's requirement: stays on the parent unless sent to a child)\n# = req {} -> \n", id, desc, id));
    }
    s
}
