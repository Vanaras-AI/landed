//! The precise (MIR) tier.
//!
//! These build a real crate with a nightly toolchain. Where nightly is absent
//! they skip rather than fail — the tier is opt-in, and CI without nightly is
//! an ordinary situation, not a broken one.

use std::path::PathBuf;
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

fn findings(dir: &PathBuf, precise: bool) -> Vec<String> {
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

fn ambiguity(dir: &PathBuf, precise: bool) -> f64 {
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
    assert!(!precise.contains(&"A::run".to_string()), "A::run is live; got {precise:?}");
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
    assert!(before > 0.0, "the fixture must be ambiguous under the nominal tier");
    assert!(after < before, "precise must reduce ambiguity: {before}% -> {after}%");
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
    assert!(!out.status.success(), "must fail rather than fall back to nominal");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("cargo crate"), "the reason must be actionable: {err}");
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
    assert!(!out.status.success(), "must fail rather than answer with less");
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
