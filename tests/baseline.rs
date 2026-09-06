//! Baseline behaviour.
//!
//! The contract a team relies on when they adopt this in CI: an unchanged
//! codebase must stay green, one new unreachable function must go red, and
//! neither must depend on line numbers, since unrelated edits shift those.

use landed::baseline::{compare, Baseline, Entry, Mode};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_landed");

fn e(name: &str, file: &str) -> Entry {
    Entry {
        name: name.into(),
        file: file.into(),
    }
}

#[test]
fn an_unchanged_codebase_reports_nothing_new() {
    let base = Baseline::new(Mode::Direct, [e("a", "src/x.rs"), e("b", "src/y.rs")]);
    let c = compare(&base, &[e("a", "src/x.rs"), e("b", "src/y.rs")]);
    assert!(c.added.is_empty());
    assert!(c.cleared.is_empty());
    assert_eq!(c.carried, 2);
}

#[test]
fn a_new_finding_is_reported() {
    let base = Baseline::new(Mode::Direct, [e("a", "src/x.rs")]);
    let c = compare(&base, &[e("a", "src/x.rs"), e("new", "src/z.rs")]);
    assert_eq!(c.added, vec![e("new", "src/z.rs")]);
    assert_eq!(c.carried, 1);
}

#[test]
fn a_fixed_finding_is_reported_as_cleared() {
    let base = Baseline::new(Mode::Direct, [e("a", "src/x.rs"), e("gone", "src/y.rs")]);
    let c = compare(&base, &[e("a", "src/x.rs")]);
    assert_eq!(c.cleared, vec![e("gone", "src/y.rs")]);
    assert!(c.added.is_empty());
}

#[test]
fn the_same_function_in_two_files_is_two_findings() {
    // Keyed on name *and* file: a name alone would let a genuine new finding
    // hide behind an accepted one that happens to share its name.
    let base = Baseline::new(Mode::Direct, [e("run", "src/a.rs")]);
    let c = compare(&base, &[e("run", "src/a.rs"), e("run", "src/b.rs")]);
    assert_eq!(c.added, vec![e("run", "src/b.rs")]);
}

#[test]
fn a_baseline_round_trips_through_disk() {
    let dir = std::env::temp_dir().join(format!("landed-bt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("b.json");
    let base = Baseline::new(Mode::Graph, [e("a", "src/x.rs")]);
    base.save(&path).unwrap();
    let back = Baseline::load(&path).unwrap();
    assert_eq!(back.version, 1);
    assert_eq!(back.mode, Mode::Graph);
    assert_eq!(back.accepted, base.accepted);
    // The timestamp should be readable, not a raw epoch count.
    assert!(
        back.created.contains('T') && back.created.ends_with('Z'),
        "got {}",
        back.created
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_corrupt_baseline_fails_loudly() {
    let dir = std::env::temp_dir().join(format!("landed-bad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("b.json");
    std::fs::write(&path, "{ not json").unwrap();
    assert!(
        Baseline::load(&path).is_err(),
        "must not silently accept garbage"
    );
    std::fs::write(
        &path,
        r#"{"version":99,"created":"x","mode":"direct","accepted":[]}"#,
    )
    .unwrap();
    let err = Baseline::load(&path).unwrap_err().to_string();
    assert!(
        err.contains("version"),
        "a future schema must be refused, not misread: {err}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ─── end to end ───────────────────────────────────────────────

fn scratch(name: &str, body: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("landed-e2e-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/main.rs"), body).unwrap();
    dir
}

const WITH_ONE_DEAD: &str = r#"
fn main() { live(); }
fn live() {}
pub fn dead_a() {}
#[cfg(test)]
mod tests { #[test] fn a() { super::dead_a(); } }
"#;

const WITH_TWO_DEAD: &str = r#"
fn main() { live(); }
fn live() {}
pub fn dead_a() {}
pub fn dead_b() {}
#[cfg(test)]
mod tests {
    #[test] fn a() { super::dead_a(); }
    #[test] fn b() { super::dead_b(); }
}
"#;

#[test]
fn ci_flow_green_then_red() {
    let dir = scratch("ci", WITH_ONE_DEAD);

    let taken = Command::new(BIN)
        .args(["baseline", dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(taken.status.success());
    assert!(
        dir.join(".landed-baseline.json").is_file(),
        "baseline must be written"
    );

    // Unchanged: green.
    let clean = Command::new(BIN)
        .args(["check", dir.to_str().unwrap(), "--baseline"])
        .output()
        .unwrap();
    assert!(
        clean.status.success(),
        "an unchanged codebase must not fail CI"
    );

    // One new dead function: red, and it names it.
    std::fs::write(dir.join("src/main.rs"), WITH_TWO_DEAD).unwrap();
    let dirty = Command::new(BIN)
        .args(["check", dir.to_str().unwrap(), "--baseline"])
        .output()
        .unwrap();
    assert_eq!(
        dirty.status.code(),
        Some(1),
        "new unreachable code must fail CI"
    );
    let out = String::from_utf8_lossy(&dirty.stdout);
    assert!(
        out.contains("dead_b"),
        "the new finding must be named; got: {out}"
    );
    assert!(
        !out.contains("dead_a"),
        "accepted findings must not be re-reported; got: {out}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_baseline_from_the_other_mode_is_refused() {
    // Comparing a graph baseline against per-function findings would report
    // the difference between two analyses as if the code had changed.
    let dir = scratch("mode", WITH_ONE_DEAD);
    Command::new(BIN)
        .args(["baseline", dir.to_str().unwrap(), "--graph"])
        .output()
        .unwrap();
    let out = Command::new(BIN)
        .args(["check", dir.to_str().unwrap(), "--baseline"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "mismatched modes must not silently compare"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.to_lowercase().contains("mode"), "got: {err}");
    std::fs::remove_dir_all(&dir).ok();
}
