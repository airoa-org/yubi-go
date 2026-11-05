//! Principle 1 of `src/README.md`, as a test rather than as good intentions.
//!
//! > Imports go to siblings or ancestors only. No module reaches into another
//! > subtree's children.
//!
//! Every violation of this was written by someone who meant well; a rule
//! nobody checks is a rule that decays one reasonable exception at a time.
//! So every import that leaves its subtree is measured, and the test fails on
//! one that is not in [`ALLOWED`] — which is empty, and meant to stay that
//! way. A line added there is a decision made out loud, in a diff.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// `app` and `hub` are ancestors of every subtree, so reaching into one is
/// what they are for.
///
/// `misc` is exempt for a different reason: it is where principle 1 *sends* a
/// module that more than one subtree needs, so importing it is the rule being
/// obeyed rather than broken. That is the whole meaning of the name here — it
/// is not "anything we could not place", and a module belongs in it only when
/// two subtrees genuinely share it.
const ANCESTORS: [&str; 3] = ["app", "hub", "misc"];

/// Imports that cross into another subtree's children and are tolerated,
/// as `importer -> imported`. Such an import is usually a module filed under
/// its first caller instead of its subject, or a display reading `record/`
/// directly where it could read through `hub`.
///
/// Adding a line here is allowed. It is a decision to make out loud, in a
/// diff, with the reason in the commit — not something that happens because
/// a path autocompleted.
const ALLOWED: [&str; 0] = [];

#[test]
fn no_module_reaches_into_another_subtree_it_has_not_already_reached_into() {
    let found = violations(Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
    let allowed: BTreeSet<String> = ALLOWED.iter().map(|s| s.to_string()).collect();

    let added: Vec<&String> = found.difference(&allowed).collect();
    assert!(
        added.is_empty(),
        "these imports cross into another subtree's children, and are not in \
         tests/layout.rs's list:\n  {}\n\nMove the module to `misc/` if it is \
         shared, or reach through `hub`. If neither fits, add the line to \
         ALLOWED and say why in the commit.",
        added.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n  ")
    );

    let fixed: Vec<&String> = allowed.difference(&found).collect();
    assert!(
        fixed.is_empty(),
        "these are listed in tests/layout.rs but no longer exist — delete \
         them, the list only shrinks:\n  {}",
        fixed.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n  ")
    );
}

/// Every `crate::<subtree>::<module>` that leaves the importer's own subtree.
///
/// Text, not syntax: a parser would be exact about a question a grep answers,
/// and this only has to be exact about `crate::` paths, which the compiler
/// already forces to be spelled out.
fn violations(src: PathBuf) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for file in rust_files(&src) {
        // `view/panel/ui.rs` is `view/panel` here: a directory module is
        // one module, or the list grows a row per leaf that says nothing new.
        let path = file.strip_prefix(&src).unwrap_or(&file).with_extension("");
        let parts: Vec<String> =
            path.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
        let (subtree, module) = match parts.as_slice() {
            [one] => (one.clone(), one.clone()),
            [head, rest @ ..] => (head.clone(), format!("{head}/{}", rest[0])),
            [] => continue,
        };
        // `app` and `hub` are the ancestors every subtree hangs from, so
        // reaching down is their job, not a violation of it.
        if module.ends_with("/mod") || ANCESTORS.contains(&module.as_str()) || module == "lib" {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        for reached in reached_subtrees(&text) {
            let (head, _) = reached.split_once("::").unwrap_or((reached.as_str(), ""));
            if head == subtree || ANCESTORS.contains(&head) {
                continue;
            }
            out.insert(format!("{module} -> {reached}"));
        }
    }
    out
}

/// `crate::a::b` as `a::b`, ignoring anything shallower — `crate::hub` alone
/// names an ancestor, which is always allowed.
fn reached_subtrees(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (at, _) in text.match_indices("crate::") {
        let rest = &text[at + "crate::".len()..];
        let path: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':')
            .collect();
        let mut names = path.split("::").filter(|s| !s.is_empty());
        let (Some(head), Some(next)) = (names.next(), names.next()) else { continue };
        // Only lowercase names are modules; `crate::hub::Hub` is a type.
        if !next.chars().next().is_some_and(|c| c.is_ascii_lowercase()) {
            continue;
        }
        out.insert(format!("{head}::{next}"));
    }
    out
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(rust_files(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    out
}
