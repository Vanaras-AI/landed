//! Which tests a change can reach.
//!
//! The audit and this question fail safely in opposite directions. Reporting
//! dead code must never accuse working code, so an edge the frontend is
//! unsure of is dropped. Choosing which tests to run must never omit a test
//! that would have caught the bug, so the same unsure edge must be kept.
//!
//! Every test here exists because the mutation harness in
//! `scripts/soundness.py` caught the analysis omitting a test that failed.

use landed::scan;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

fn scratch(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("landed-impact-{name}-{}-{n}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn crate_with(dir: &Path, lib: &str, integration: &str) {
    write(
        dir,
        "Cargo.toml",
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(dir, "src/lib.rs", lib);
    write(dir, "tests/it.rs", integration);
}

fn affected(dir: &Path, symbol: &str) -> Vec<String> {
    let s = scan::scan_crate(dir).unwrap();
    let targets: HashSet<String> = s
        .defs
        .iter()
        .filter(|d| d.name() == symbol)
        .map(|d| d.key())
        .collect();
    assert!(
        !targets.is_empty(),
        "{symbol} is not defined in the fixture"
    );
    scan::tests_reaching(&s, &targets)
}

/// The ordinary case: a test that calls the changed function is selected, and
/// one that cannot reach it is not.
#[test]
fn a_test_that_reaches_the_change_is_selected_and_others_are_not() {
    let dir = scratch("basic");
    crate_with(
        &dir,
        "pub fn changed() -> u8 { 1 }\npub fn untouched() -> u8 { 2 }\n",
        "#[test]\nfn hits() { assert_eq!(demo::changed(), 1); }\n\
         #[test]\nfn misses() { assert_eq!(demo::untouched(), 2); }\n",
    );
    let hit = affected(&dir, "changed");
    assert!(hit.contains(&"hits".to_string()), "got {hit:?}");
    assert!(!hit.contains(&"misses".to_string()), "got {hit:?}");
}

/// Nearly every Rust test does its work inside `assert!`, and a token inside a
/// macro may be a call or a tuple-struct literal. The audit drops that edge so
/// it cannot invent a finding; selection must keep it, or the tests that
/// actually exercise the change are the ones dropped.
#[test]
fn a_call_made_inside_an_assertion_still_selects_the_test() {
    let dir = scratch("macro");
    crate_with(
        &dir,
        "pub fn changed() -> bool { true }\n",
        "#[test]\nfn only_inside_a_macro() { assert!(demo::changed()); }\n",
    );
    let hit = affected(&dir, "changed");
    assert!(
        hit.contains(&"only_inside_a_macro".to_string()),
        "got {hit:?}"
    );
}

/// A test that runs the program as a subprocess exercises all of it through a
/// boundary no call edge crosses. Read as "reaches nothing", it would be
/// skipped for every change — which is exactly the test that catches
/// integration bugs.
#[test]
fn a_test_that_spawns_a_process_always_runs() {
    let dir = scratch("opaque");
    crate_with(
        &dir,
        "pub fn changed() -> u8 { 1 }\npub fn unrelated() -> u8 { 2 }\n",
        "use std::process::Command;\n\
         #[test]\nfn end_to_end() { let _ = Command::new(\"demo\").output(); }\n",
    );
    // `changed` is not called anywhere in that test, and it is still selected.
    let hit = affected(&dir, "changed");
    assert!(hit.contains(&"end_to_end".to_string()), "got {hit:?}");
}

/// A test rarely spawns the process itself: it calls a helper that does. The
/// marker sits on the helper, so opacity has to travel back up to the test.
/// Missing this left 20 integration tests unselected on this very repository.
#[test]
fn opacity_propagates_from_a_helper_to_its_callers() {
    let dir = scratch("propagate");
    crate_with(
        &dir,
        "pub fn changed() -> u8 { 1 }\n",
        "use std::process::Command;\n\
         fn run() -> String { String::from_utf8(Command::new(\"demo\").output().unwrap().stdout).unwrap() }\n\
         #[test]\nfn drives_the_binary() { let _ = run(); }\n",
    );
    let hit = affected(&dir, "changed");
    assert!(
        hit.contains(&"drives_the_binary".to_string()),
        "got {hit:?}"
    );
}

/// The permissive graph must not leak into the audit. Keeping macro edges out
/// of the accusable graph is the whole reason the audit does not invent
/// findings from token matches inside tests.
#[test]
fn the_audit_graph_stays_conservative() {
    let dir = scratch("split");
    crate_with(
        &dir,
        "pub fn only_a_macro_mentions_me() -> bool { true }\n",
        "#[test]\nfn t() { assert!(demo::only_a_macro_mentions_me()); }\n",
    );
    let s = scan::scan_crate(&dir).unwrap();
    let key = s
        .defs
        .iter()
        .find(|d| d.name() == "only_a_macro_mentions_me")
        .unwrap()
        .key();

    let roots: HashSet<String> = s
        .defs
        .iter()
        .filter(|d| d.is_test_fn)
        .map(|d| d.key())
        .collect();
    assert!(
        !scan::reachable_over(&s, &roots, scan::Graph::Accusable).contains(&key),
        "a macro token in test code must not reach the audit's graph"
    );
    assert!(
        scan::reachable_over(&s, &roots, scan::Graph::Everything).contains(&key),
        "and must be present in the permissive one"
    );
}
