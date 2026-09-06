//! The precise (MIR) tier.
//!
//! These build a real crate with a nightly toolchain. Where nightly is absent
//! they skip rather than fail — the tier is opt-in, and CI without nightly is
//! an ordinary situation, not a broken one.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_landed");

fn have_nightly() -> bool {
    Command::new("cargo")
        .args(["+nightly", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn scratch(name: &str, main_rs: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("landed-precise-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"pt\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/main.rs"), main_rs).unwrap();
    dir
}

fn findings(dir: &Path, precise: bool) -> Vec<String> {
    let mut args = vec!["check", dir.to_str().unwrap(), "--graph", "--json"];
    if precise {
        args.push("--precise");
    }
    let out = Command::new(BIN).args(&args).output().unwrap();
    let v: serde_json::Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let mut names: Vec<String> = Vec::new();
    if let Some(regions) = v["regions"].as_array() {
        for r in regions {
            names.push(r["entry"]["name"].as_str().unwrap_or("").to_string());
            if let Some(ms) = r["members"].as_array() {
                for m in ms {
                    names.push(m["name"].as_str().unwrap_or("").to_string());
                }
            }
        }
    }
    names.sort();
    names
}

fn ambiguity(dir: &Path, precise: bool) -> f64 {
    let mut args = vec!["check", dir.to_str().unwrap(), "--stats"];
    if precise {
        args.push("--precise");
    }
    let out = Command::new(BIN).args(&args).output().unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines()
        .find(|l| l.contains("non-unique"))
        .and_then(|l| l.split('(').nth(1))
        .and_then(|l| l.trim_end_matches("%)").parse().ok())
        .unwrap_or(-1.0)
}

/// `A::run` is live; `B::run` is reached only from a test. Under a name-keyed
/// graph both are called `run`, so neither may be judged and the dead one is
/// invisible. This is the case the tier exists for.
const SAME_NAME_METHODS: &str = r#"
struct A;
struct B;
impl A { pub fn run(&self) -> u8 { helper_a() } }
impl B { pub fn run(&self) -> u8 { helper_b() } }
fn helper_a() -> u8 { 1 }
fn helper_b() -> u8 { 2 }
fn main() { let _ = A.run(); }
#[cfg(test)]
mod tests { #[test] fn t() { let _ = super::B.run(); } }
"#;

#[test]
fn same_named_methods_on_different_types_are_distinguished() {
    if !have_nightly() {
        eprintln!("skipping: nightly not installed");
        return;
    }
    let dir = scratch("methods", SAME_NAME_METHODS);

    // Default: the name is ambiguous, so nothing is reported — a false
    // negative the syntactic tier cannot avoid.
    assert!(
        findings(&dir, false).is_empty(),
        "the nominal tier must decline to judge an ambiguous name"
    );

    // Precise: B::run and its helper are reported, named by type.
    let precise = findings(&dir, true);
    assert!(precise.contains(&"B::run".to_string()), "got {precise:?}");
    assert!(
        !precise.contains(&"A::run".to_string()),
        "A::run is live; got {precise:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn precise_reduces_measured_ambiguity() {
    if !have_nightly() {
        eprintln!("skipping: nightly not installed");
        return;
    }
    let dir = scratch("amb", SAME_NAME_METHODS);
    let (before, after) = (ambiguity(&dir, false), ambiguity(&dir, true));
    assert!(
        before > 0.0,
        "the fixture must be ambiguous under the nominal tier"
    );
    assert!(
        after < before,
        "precise must reduce ambiguity: {before}% -> {after}%"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_crate_with_a_lib_and_a_bin_is_dumped_per_target() {
    // `cargo rustc` passes trailing arguments to one target only, so asking
    // for all of them at once fails with a message about argument passing
    // that says nothing about the code.
    if !have_nightly() {
        eprintln!("skipping: nightly not installed");
        return;
    }
    let dir = scratch("multi", "fn main() { lib_side(); }\nfn lib_side() {}\n");
    std::fs::write(dir.join("src/lib.rs"), "pub fn from_lib() {}\n").unwrap();
    let out = Command::new(BIN)
        .args(["check", dir.to_str().unwrap(), "--stats", "--precise"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "lib + bin must not fail: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ─── failure modes: precise must never quietly degrade ────────

#[test]
fn precise_refuses_a_path_that_is_not_a_cargo_crate() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/dead_region/src");
    let out = Command::new(BIN)
        .args(["check", fixtures.to_str().unwrap(), "--precise"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "must fail rather than fall back to nominal"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("cargo crate"),
        "the reason must be actionable: {err}"
    );
}

#[test]
fn precise_refuses_a_crate_that_does_not_compile() {
    if !have_nightly() {
        eprintln!("skipping: nightly not installed");
        return;
    }
    let dir = scratch("broken", "fn main() { this_does_not_exist(); }\n");
    let out = Command::new(BIN)
        .args(["check", dir.to_str().unwrap(), "--precise"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "must fail rather than answer with less"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("could not produce MIR") || err.contains("compile"),
        "the reason must be actionable: {err}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_default_tier_is_unaffected_by_the_precise_one_existing() {
    // The normal invocation must stay fast and independent of MIR: no build,
    // no toolchain requirement.
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/dead_region/src");
    let out = Command::new(BIN)
        .args(["check", fixtures.to_str().unwrap(), "--graph"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("dead_entry"));
}

// ─── same-name collisions (brief cases A–D) ───────────────────

/// Case A — two free functions sharing a name in different modules.
/// `alpha::helper` is live; `beta::helper` is reached only from a test.
const CASE_A: &str = r#"
mod alpha { pub fn helper() -> u8 { 1 } }
mod beta  { pub fn helper() -> u8 { 2 } }
fn main() { let _ = alpha::helper(); }
#[cfg(test)]
mod tests { #[test] fn t() { let _ = super::beta::helper(); } }
"#;

#[test]
fn case_a_same_named_free_functions_are_not_merged() {
    if !have_nightly() {
        eprintln!("skipping: nightly not installed");
        return;
    }
    let dir = scratch("case_a", CASE_A);
    let found = findings(&dir, true);
    // They must not merge: merging would make beta::helper look live because
    // alpha::helper is called, and the dead one would vanish.
    assert!(
        found.iter().any(|n| n.contains("beta")),
        "beta::helper is test-only and must be reported; got {found:?}"
    );
    assert!(
        !found.iter().any(|n| n.contains("alpha")),
        "alpha::helper is live and must not be; got {found:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Case B — a production function and a test function sharing a name.
/// `worker` is live in production; a same-named `worker` exists in the test
/// module. Classifying by bare name would mark the production caller's edges
/// as test edges and strand everything it calls.
const CASE_B: &str = r#"
fn worker() -> u8 { deep_production() }
fn deep_production() -> u8 { 1 }
fn main() { let _ = worker(); }
#[cfg(test)]
mod tests {
    fn worker() -> u8 { 99 }
    #[test] fn t() { let _ = worker(); }
}
"#;

#[test]
fn case_b_a_name_shared_by_production_and_test_does_not_strand_production() {
    if !have_nightly() {
        eprintln!("skipping: nightly not installed");
        return;
    }
    let dir = scratch("case_b", CASE_B);
    let found = findings(&dir, true);
    assert!(
        !found.iter().any(|n| n.contains("deep_production")),
        "deep_production is reached from main via worker; a name collision \
         with the test-module worker must not reclassify that edge: got {found:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Case C — the same method name on two types, both live.
const CASE_C: &str = r#"
struct A; struct B;
impl A { pub fn process(&self) -> u8 { from_a() } }
impl B { pub fn process(&self) -> u8 { from_b() } }
fn from_a() -> u8 { 1 }
fn from_b() -> u8 { 2 }
fn main() { let _ = A.process(); let _ = B.process(); }
"#;

#[test]
fn case_c_same_method_name_on_different_types_both_live() {
    if !have_nightly() {
        eprintln!("skipping: nightly not installed");
        return;
    }
    let dir = scratch("case_c", CASE_C);
    let found = findings(&dir, true);
    assert!(
        found.is_empty(),
        "both are reached from main; got {found:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Case D — one method reached from production, its twin only from a test.
const CASE_D: &str = r#"
struct A; struct B;
impl A { pub fn process(&self) -> u8 { only_a() } }
impl B { pub fn process(&self) -> u8 { only_b() } }
fn only_a() -> u8 { 1 }
fn only_b() -> u8 { 2 }
fn main() { let _ = A.process(); }
#[cfg(test)]
mod tests { #[test] fn t() { let _ = super::B.process(); } }
"#;

#[test]
fn case_d_production_and_test_reachability_are_distinguished() {
    if !have_nightly() {
        eprintln!("skipping: nightly not installed");
        return;
    }
    let dir = scratch("case_d", CASE_D);
    let found = findings(&dir, true);
    assert!(found.contains(&"B::process".to_string()), "got {found:?}");
    assert!(
        found.contains(&"only_b".to_string()),
        "downstream too; got {found:?}"
    );
    assert!(
        !found.contains(&"A::process".to_string()),
        "A is live; got {found:?}"
    );
    assert!(
        !found.contains(&"only_a".to_string()),
        "reached via A; got {found:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn precise_never_labels_a_symbol_resolved() {
    // The tier narrows identity; it does not prove global uniqueness. Claiming
    // Resolved would assert something the MIR dump cannot support.
    use landed::ir::{Precision, SymbolId};
    assert_eq!(SymbolId::typed("f", "A").precision(), Precision::Typed);
    assert_eq!(SymbolId::in_module("f", "m").precision(), Precision::Typed);
    assert_eq!(SymbolId::nominal("f").precision(), Precision::Nominal);
}
