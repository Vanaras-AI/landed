//! Every flag must do something.
//!
//! `--stats` once parsed, bound to a variable, appeared in `--help`, and fell
//! through to the default report because its handler had been deleted while
//! editing the block beside it. It shipped twice. rustc warned on every build
//! — "unused variable: `stats`" — and the build command in use filtered
//! output to lines matching `^error`.
//!
//! A flag that produces the default output is indistinguishable from a flag
//! that does nothing, so each of these asserts on something only that flag
//! can produce.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_landed");

fn fixture(name: &str) -> String {
    format!("{}/fixtures/{name}/src", env!("CARGO_MANIFEST_DIR"))
}

fn run(args: &[&str]) -> String {
    let out = Command::new(BIN).args(args).output().expect("binary should run");
    assert!(
        out.status.success() || out.status.code() == Some(1),
        "unexpected exit: {:?}",
        out.status
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn plain_check_reports_findings() {
    let o = run(&["check", &fixture("test_only")]);
    assert!(o.contains("only_tests_call_me"), "got: {o}");
    assert!(o.contains("NEVER RUN"), "got: {o}");
}

#[test]
fn graph_groups_into_regions_and_names_the_frontier() {
    let o = run(&["check", &fixture("dead_region"), "--graph"]);
    assert!(o.contains("Region 1"), "got: {o}");
    assert!(o.contains("frontier"), "got: {o}");
    assert!(o.contains("dead_entry"), "got: {o}");
}

#[test]
fn flat_bypasses_regions() {
    let o = run(&["check", &fixture("dead_region"), "--graph", "--flat"]);
    assert!(o.contains("NEVER RUN"), "expected the flat listing; got: {o}");
    assert!(!o.contains("Region 1"), "--flat must not group; got: {o}");
}

#[test]
fn stats_reports_coverage_and_nothing_else() {
    let o = run(&["check", &fixture("ambiguous"), "--stats"]);
    assert!(o.contains("non-unique names"), "--stats did nothing; got: {o}");
    assert!(
        !o.contains("NEVER RUN") && !o.contains("Region 1"),
        "--stats must not fall through to a report; got: {o}"
    );
}

#[test]
fn dot_emits_a_graph_and_nothing_else() {
    let o = run(&["check", &fixture("dead_region"), "--dot"]);
    assert!(o.starts_with("digraph calls {"), "got: {o}");
    assert!(o.trim_end().ends_with('}'), "got: {o}");
    assert!(!o.contains("landed v"), "--dot must emit only DOT; got: {o}");
}

#[test]
fn explain_shows_evidence_for_one_symbol() {
    let o = run(&["check", &fixture("dead_region"), "--explain", "dead_entry"]);
    assert!(o.contains("status"), "got: {o}");
    assert!(o.contains("call sites"), "got: {o}");
    assert!(o.contains("callers"), "got: {o}");
    assert!(!o.contains("Region 1"), "--explain must not fall through; got: {o}");
}

#[test]
fn json_is_parseable_json() {
    let o = run(&["check", &fixture("test_only"), "--json"]);
    let v: serde_json::Value = serde_json::from_str(&o).expect("must be valid JSON");
    assert!(v.is_array(), "findings should be an array; got: {o}");
}

#[test]
fn graph_json_is_parseable_json() {
    let o = run(&["check", &fixture("dead_region"), "--graph", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&o).expect("must be valid JSON");
    assert!(v[0]["entry"]["name"].is_string(), "regions need a frontier; got: {o}");
    assert!(v[0]["confidence"].is_string(), "regions need a confidence; got: {o}");
}

#[test]
fn fail_over_gates_the_exit_code() {
    let clean = Command::new(BIN)
        .args(["check", &fixture("all_live"), "--fail-over", "0"])
        .output()
        .unwrap();
    assert!(clean.status.success(), "a clean crate must not fail the build");

    let dirty = Command::new(BIN)
        .args(["check", &fixture("dead_region"), "--graph", "--fail-over", "1"])
        .output()
        .unwrap();
    assert_eq!(
        dirty.status.code(),
        Some(1),
        "three findings over a threshold of one must exit non-zero"
    );
}

#[test]
fn a_clean_crate_says_so_rather_than_printing_nothing() {
    let o = run(&["check", &fixture("all_live"), "--graph"]);
    assert!(o.contains("reachable"), "got: {o}");
}
