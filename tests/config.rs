//! Developer-declared entry points and ignores.
//!
//! No heuristic finds every entry point. A handler spawned as a task or held
//! in a registry breaks the chain from `main`, and everything downstream is
//! then condemned — on one real codebase that wrongly included a live
//! PII-redaction subsystem. `landed.toml` lets the developer state what the
//! analyzer cannot infer, and their statement outranks the heuristic.

use landed::scan;
use std::path::{Path, PathBuf};

fn scratch(name: &str, main_rs: &str, config: Option<&str>) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("landed-cfg-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/main.rs"), main_rs).unwrap();
    if let Some(c) = config {
        std::fs::write(dir.join("landed.toml"), c).unwrap();
    }
    dir
}

/// `handle_webhook` is genuinely live, but it is entered through a spawn the
/// analyzer cannot follow, so both it and `do_work` look dead.
const SPAWNED_ENTRY: &str = r#"
fn main() { spawn_it(); }
fn spawn_it() { /* tokio::spawn(handle_webhook()) */ }
pub fn handle_webhook() { do_work(); }
fn do_work() {}
#[cfg(test)]
mod tests { #[test] fn t() { super::handle_webhook(); } }
"#;

fn dead_names(dir: &Path) -> Vec<String> {
    let s = scan::scan_crate(dir).unwrap();
    let mut v: Vec<String> = scan::never_run_graph(&s)
        .into_iter()
        .map(|f| f.name)
        .collect();
    v.sort();
    v
}

#[test]
fn without_config_a_spawned_entry_point_is_wrongly_condemned() {
    // Establishes the problem the config exists to solve. If this ever stops
    // failing, the analyzer got better and the test should be revisited.
    let dir = scratch("none", SPAWNED_ENTRY, None);
    let names = dead_names(&dir);
    assert!(
        names.contains(&"handle_webhook".to_string()),
        "got {names:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_declared_root_makes_it_and_everything_downstream_live() {
    let dir = scratch(
        "root",
        SPAWNED_ENTRY,
        Some("roots = [\"handle_webhook\"]\n"),
    );
    assert!(
        dead_names(&dir).is_empty(),
        "declaring the entry point must rescue the whole region beneath it"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_wildcard_root_matches() {
    let dir = scratch("glob", SPAWNED_ENTRY, Some("roots = [\"handle_*\"]\n"));
    assert!(dead_names(&dir).is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn ignored_names_are_never_reported() {
    let dir = scratch(
        "ign",
        SPAWNED_ENTRY,
        Some("ignore = [\"handle_*\", \"do_work\"]\n"),
    );
    assert!(dead_names(&dir).is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_ignore_does_not_silence_unrelated_findings() {
    let dir = scratch("ign2", SPAWNED_ENTRY, Some("ignore = [\"do_work\"]\n"));
    let names = dead_names(&dir);
    assert!(
        names.contains(&"handle_webhook".to_string()),
        "got {names:?}"
    );
    assert!(!names.contains(&"do_work".to_string()), "got {names:?}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_malformed_config_does_not_silently_disable_itself() {
    // Falling back to "no config" on a typo would silently drop the roots the
    // developer declared, and the analyzer would start condemning live code
    // again with no indication why.
    let dir = scratch("bad", SPAWNED_ENTRY, Some("roots = \"not a list\"\n"));
    let cfg = landed::config::Config::load(&dir);
    assert!(
        cfg.is_err(),
        "a malformed landed.toml must be an error, not a default"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_unknown_key_is_rejected_rather_than_ignored() {
    // A misspelled key that is silently accepted is a config that does
    // nothing while appearing to work.
    let dir = scratch("unk", SPAWNED_ENTRY, Some("rootz = [\"handle_webhook\"]\n"));
    assert!(
        landed::config::Config::load(&dir).is_err(),
        "typo must be caught"
    );
    std::fs::remove_dir_all(&dir).ok();
}
