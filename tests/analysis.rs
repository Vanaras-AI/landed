//! Behavioural corpus.
//!
//! Fixtures live in `fixtures/` at the repo root, not under `tests/`: the
//! analyzer treats any path containing `/tests/` as test code, so fixtures
//! stored there scan as empty and every assertion silently passes on nothing.
//!
//! Every case here is a bug that shipped at least once, or a suppression the
//! tool promises to honour. The false-positive cases matter more than the
//! true-positive ones: a missed finding costs one bug, a wrong accusation
//! costs the user's trust in every finding after it.

use landed::scan::{self, Confidence};
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
        .join("src")
}

fn scan_of(name: &str) -> scan::Scan {
    scan::scan_crate(&fixture(name)).expect("fixture should scan")
}

fn flagged(name: &str) -> Vec<String> {
    let s = scan_of(name);
    let mut v: Vec<String> = scan::never_run(&s).into_iter().map(|f| f.name).collect();
    v.sort();
    v
}

fn flagged_graph(name: &str) -> Vec<String> {
    let s = scan_of(name);
    let mut v: Vec<String> = scan::never_run_graph(&s).into_iter().map(|f| f.name).collect();
    v.sort();
    v
}

// ─── true positives ───────────────────────────────────────────

#[test]
fn reports_a_function_only_tests_call() {
    assert_eq!(flagged("test_only"), vec!["only_tests_call_me"]);
}

#[test]
fn graph_mode_reports_the_whole_dead_region_not_just_its_tip() {
    // The per-function check sees only the outermost function: `middle` and
    // `leaf` each have a caller, and that caller merely happens to be dead.
    assert_eq!(flagged("dead_region"), vec!["dead_entry"]);
    // Reachability sees all three.
    assert_eq!(flagged_graph("dead_region"), vec!["dead_entry", "leaf", "middle"]);
}

#[test]
fn a_dead_region_is_grouped_with_its_frontier_named() {
    let s = scan_of("dead_region");
    let regions = scan::dead_regions(&s);
    assert_eq!(regions.len(), 1, "three functions, one subsystem");
    let r = &regions[0];
    assert_eq!(r.size, 3);
    assert_eq!(r.entry.name, "dead_entry", "the frontier is the way in");
    assert_eq!(
        r.confidence,
        Confidence::High,
        "the frontier has no production caller, so the region is not in doubt"
    );
}

// ─── false positives: each of these shipped once ──────────────

#[test]
fn everything_reachable_reports_nothing() {
    assert!(flagged("all_live").is_empty());
    assert!(flagged_graph("all_live").is_empty());
}

#[test]
fn a_doc_comment_saying_test_is_not_a_test_attribute() {
    // Regression: doc comments are attributes in syn, so a substring match for
    // "test" on an attribute's tokens matched `/// is a test hook`, `fastest`
    // and `latest`. Every call inside such a function was then counted as a
    // test call, erasing its real production callers.
    assert!(
        flagged("doc_mentions_test").is_empty(),
        "real_work is called by driver, which main calls"
    );
}

#[test]
fn calls_inside_macro_bodies_are_seen() {
    // Regression: syn treats a macro invocation as an opaque token stream, so
    // a call written inside one was invisible and its callee looked dead.
    assert!(
        flagged_graph("macro_call").is_empty(),
        "called_from_macro is invoked inside wrap!{{...}}"
    );
}

#[test]
fn helpers_in_a_tests_rs_file_are_test_code() {
    // Regression: `tests/` and `*_test.rs` were handled, `src/**/tests.rs`
    // was not — so every helper in that file counted as shipped production.
    let names = flagged("tests_rs");
    assert!(
        !names.contains(&"make_fixture".to_string()),
        "make_fixture lives in tests.rs; got {names:?}"
    );
}

#[test]
fn a_library_public_api_is_an_entry_point() {
    // Regression: a library has no `main`, and one that writes `pub mod foo;`
    // rather than `pub use foo::…` had an empty root set — so the entire
    // crate was reported unreachable. One 120-function crate came back at 51%.
    assert!(
        flagged_graph("lib_api").is_empty(),
        "public_api is the crate's entry point; helper is reached through it"
    );
}

#[test]
fn trait_impl_methods_are_never_accused() {
    // Dispatch through `dyn Trait` is invisible to a name-keyed graph, so the
    // absence of a direct call site proves nothing.
    let names = flagged_graph("trait_impl");
    assert!(!names.contains(&"greet".to_string()), "got {names:?}");
}

#[test]
fn allow_dead_code_is_respected() {
    assert!(flagged("allow_dead").is_empty(), "the author already decided");
}

#[test]
fn a_name_defined_twice_is_not_judged() {
    // `A::process` and `B::process` are distinct symbols that a name-keyed
    // graph cannot tell apart, so it must say nothing about either.
    let names = flagged_graph("ambiguous");
    assert!(!names.contains(&"process".to_string()), "got {names:?}");
}

// ─── the confidence contract ──────────────────────────────────

#[test]
fn confidence_is_high_only_when_no_production_caller_exists() {
    for f in scan::never_run(&scan_of("test_only")) {
        assert_eq!(f.confidence, Confidence::High);
        assert_eq!(f.prod_calls, 0, "High confidence requires zero production callers");
    }
}

#[test]
fn coverage_is_reported_honestly() {
    // A report that does not say what it could not see overstates itself.
    let (ambiguous, total) = scan::ambiguity_report(&scan_of("ambiguous"));
    assert!(total > 0);
    assert!(ambiguous > 0, "the ambiguous fixture must register as ambiguous");
}

// ─── evidence ─────────────────────────────────────────────────

#[test]
fn evidence_explains_a_finding() {
    let s = scan_of("dead_region");
    let e = scan::evidence(&s, "dead_entry");
    assert!(!e.in_production_set, "not reachable from main");
    assert!(e.in_test_set, "but the tests reach it");
    assert!(e.suppressed.is_none(), "nothing about it is unanalysable");
    assert_eq!(e.prod_call_sites, 0);
    assert!(e.test_call_sites > 0);
}

#[test]
fn evidence_states_why_a_suppressed_symbol_was_not_judged() {
    let s = scan_of("ambiguous");
    let e = scan::evidence(&s, "process");
    assert!(
        e.suppressed.is_some(),
        "a duplicated name must carry a stated reason for silence"
    );
}
