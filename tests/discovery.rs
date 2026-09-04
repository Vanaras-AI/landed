//! Cargo-driven discovery, baseline fingerprints, and SARIF.

use landed::{baseline, targets};
use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_landed");

fn here() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

// ─── cargo target discovery ───────────────────────────────────

#[test]
fn cargo_answers_for_a_real_crate() {
    let ws = targets::discover(&here());
    assert!(ws.from_cargo, "landed is a cargo project; cargo should answer");
    assert!(ws.is_application(), "it has a [[bin]]");
    assert!(ws.binary_names().contains(&"landed".to_string()), "{:?}", ws.binary_names());
}

#[test]
fn test_targets_are_excluded_from_production_sources() {
    // tests/ are cargo targets of kind "test". They must not be scanned as
    // production, and cargo says so authoritatively rather than by path.
    let ws = targets::discover(&here());
    let dirs = ws.production_source_dirs();
    assert!(
        !dirs.iter().any(|d| d.ends_with("tests")),
        "tests/ must not be a production source dir; got {dirs:?}"
    );
    assert!(
        ws.non_production_roots().iter().any(|d| d.ends_with("tests")),
        "tests/ should be classified as non-production"
    );
}

#[test]
fn a_directory_that_is_not_a_cargo_project_falls_back() {
    // cargo metadata walks *up* for a manifest, so a bare directory inside a
    // cargo project would otherwise be answered for by its ancestor — and the
    // analyzer would scan the wrong crate entirely.
    let fixture = here().join("fixtures/dead_region/src");
    let ws = targets::discover(&fixture);
    assert!(
        !ws.from_cargo,
        "a fixture with no manifest of its own must not inherit landed's"
    );
}

#[test]
fn discovery_never_errors_on_a_path_cargo_cannot_read() {
    let ws = targets::discover(std::path::Path::new("/nonexistent-path-xyz"));
    assert!(!ws.from_cargo, "absence of cargo is ordinary, not an error");
    assert!(ws.production_source_dirs().is_empty());
}

// ─── baseline fingerprinting ──────────────────────────────────

#[test]
fn an_unchanged_analysis_is_not_stale() {
    let cfg = landed::config::Config::default();
    let fp = baseline::Fingerprint::of(&cfg);
    let b = baseline::Baseline::with_fingerprint(baseline::Mode::Direct, [], Some(fp.clone()));
    assert!(b.staleness(&fp).is_none());
}

#[test]
fn changed_config_makes_a_baseline_stale() {
    // Editing landed.toml changes what is reachable, so findings move for
    // reasons that have nothing to do with the code.
    let before = landed::config::Config::default();
    let mut after = landed::config::Config::default();
    after.ignore.push("something".into());

    let b = baseline::Baseline::with_fingerprint(
        baseline::Mode::Direct,
        [],
        Some(baseline::Fingerprint::of(&before)),
    );
    let why = b.staleness(&baseline::Fingerprint::of(&after));
    assert!(why.is_some(), "config drift must be reported");
    assert!(why.unwrap().contains("landed.toml"), "and must say why");
}

#[test]
fn a_baseline_with_no_fingerprint_says_so() {
    // Files written before fingerprints existed cannot be checked for drift,
    // which is itself worth telling the user rather than assuming fresh.
    let b = baseline::Baseline::with_fingerprint(baseline::Mode::Direct, [], None);
    let why = b.staleness(&baseline::Fingerprint::of(&Default::default()));
    assert!(why.is_some(), "an unfingerprinted baseline is not known to be current");
}

#[test]
fn the_fingerprint_is_order_independent() {
    // Reordering entries in landed.toml is not a change in meaning.
    let mut a = landed::config::Config::default();
    a.roots = vec!["x".into(), "y".into()];
    let mut b = landed::config::Config::default();
    b.roots = vec!["y".into(), "x".into()];
    assert_eq!(
        baseline::Fingerprint::of(&a).config_digest,
        baseline::Fingerprint::of(&b).config_digest
    );
}

// ─── SARIF ────────────────────────────────────────────────────

fn sarif_of(fixture: &str, graph: bool) -> serde_json::Value {
    let path = here().join("fixtures").join(fixture).join("src");
    let mut args = vec!["check", path.to_str().unwrap(), "--format", "sarif"];
    if graph {
        args.push("--graph");
    }
    let out = Command::new(BIN).args(&args).output().unwrap();
    serde_json::from_slice(&out.stdout).expect("SARIF must be valid JSON")
}

#[test]
fn sarif_is_well_formed() {
    let v = sarif_of("dead_region", true);
    assert_eq!(v["version"], "2.1.0");
    assert!(v["$schema"].as_str().unwrap().contains("sarif"));
    let driver = &v["runs"][0]["tool"]["driver"];
    assert_eq!(driver["name"], "landed");
    assert!(driver["rules"].as_array().unwrap().len() >= 3, "rules must be declared");
}

#[test]
fn every_sarif_result_can_be_placed_in_a_file() {
    // A result without a resolvable location cannot be shown on a diff.
    let v = sarif_of("dead_region", true);
    let results = v["runs"][0]["results"].as_array().unwrap();
    assert!(!results.is_empty());
    for r in results {
        let loc = &r["locations"][0]["physicalLocation"];
        assert!(loc["artifactLocation"]["uri"].is_string(), "{r}");
        assert!(loc["region"]["startLine"].as_u64().unwrap() >= 1, "{r}");
        assert!(r["ruleId"].is_string(), "{r}");
        assert!(r["partialFingerprints"].is_object(), "results must be trackable across runs");
    }
}

#[test]
fn sarif_paths_are_relative() {
    // Absolute paths from the analysing machine do not resolve in a code
    // scanning UI on another one.
    let v = sarif_of("dead_region", true);
    let uri = v["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["artifactLocation"]
        ["uri"]
        .as_str()
        .unwrap();
    assert!(!uri.starts_with('/'), "got absolute path: {uri}");
}

#[test]
fn a_clean_crate_produces_valid_sarif_with_no_results() {
    let v = sarif_of("all_live", true);
    assert_eq!(v["version"], "2.1.0");
    assert_eq!(v["runs"][0]["results"].as_array().unwrap().len(), 0);
}
