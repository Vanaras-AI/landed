//! Crate discovery from `cargo metadata` rather than directory shape.
//!
//! The analyzer previously inferred everything from the filesystem: a
//! directory with `Cargo.toml` is a crate, one containing `main.rs` is a
//! binary, a path containing `/tests/` or `/examples/` is not production.
//! Each of those is usually right and occasionally wrong, and each wrong
//! answer is expensive — treating a library as an application condemns its
//! whole public API, and missing a `[[bin]]` loses the only real entry point.
//!
//! Cargo already knows. It knows every target, its kind, and the exact file
//! it starts from, including targets declared explicitly in the manifest that
//! live nowhere the convention would predict.
//!
//! Shelling out rather than linking `cargo_metadata` keeps the dependency
//! count where it is; the subset of the schema this needs is small and stable.

use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A real entry point: `main` runs.
    Bin,
    /// Consumed by other crates, so its public API is an entry point.
    Lib,
    /// Integration test, benchmark, example. Compiled, but never production.
    NotProduction,
    /// `build.rs`. Runs at build time, not in the shipped program.
    BuildScript,
}

impl Kind {
    fn from_cargo(kinds: &[String]) -> Self {
        // A target may carry several kinds (lib, rlib, cdylib). Precedence
        // matters: anything that is a bin is an entry point.
        if kinds.iter().any(|k| k == "bin") {
            Kind::Bin
        } else if kinds.iter().any(|k| k == "custom-build") {
            Kind::BuildScript
        } else if kinds.iter().any(|k| k == "test" || k == "bench" || k == "example") {
            Kind::NotProduction
        } else {
            Kind::Lib
        }
    }
}

#[derive(Debug, Clone)]
pub struct Target {
    pub name: String,
    pub kind: Kind,
    /// The file cargo compiles this target from.
    pub src_path: PathBuf,
    /// Package this target belongs to.
    pub package: String,
}

#[derive(Debug, Default, Clone)]
pub struct Workspace {
    pub targets: Vec<Target>,
    /// True when cargo answered. False means every field here is a guess and
    /// the caller should fall back to directory heuristics.
    pub from_cargo: bool,
}

impl Workspace {
    /// Directories to scan: the parent of every production target's entry
    /// file. Test, bench, example and build-script targets are excluded by
    /// cargo's own classification rather than by path matching.
    pub fn production_source_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = self
            .targets
            .iter()
            .filter(|t| matches!(t.kind, Kind::Bin | Kind::Lib))
            .filter_map(|t| t.src_path.parent().map(Path::to_path_buf))
            .collect();
        dirs.sort();
        dirs.dedup();
        // A nested dir whose ancestor is already scanned would be walked twice.
        let mut out: Vec<PathBuf> = Vec::new();
        for d in dirs {
            if !out.iter().any(|o| d.starts_with(o)) {
                out.push(d);
            }
        }
        out
    }

    /// Files cargo compiles as tests, benches or examples. Anything defined
    /// in one of these is test code however it is named or wherever it lives.
    pub fn non_production_roots(&self) -> Vec<PathBuf> {
        self.targets
            .iter()
            .filter(|t| t.kind == Kind::NotProduction)
            .filter_map(|t| t.src_path.parent().map(Path::to_path_buf))
            .collect()
    }

    /// Is this an application? Answered by whether cargo declares a binary,
    /// not by whether a file called `main.rs` happens to exist.
    pub fn is_application(&self) -> bool {
        self.targets.iter().any(|t| t.kind == Kind::Bin)
    }

    /// Entry-point functions cargo can name for us: every binary's `main`.
    pub fn binary_names(&self) -> Vec<String> {
        self.targets
            .iter()
            .filter(|t| t.kind == Kind::Bin)
            .map(|t| t.name.clone())
            .collect()
    }
}

// ─── cargo metadata ───────────────────────────────────────────

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_members: Vec<String>,
}

#[derive(Deserialize)]
struct Package {
    id: String,
    name: String,
    targets: Vec<CargoTarget>,
}

#[derive(Deserialize)]
struct CargoTarget {
    name: String,
    kind: Vec<String>,
    src_path: PathBuf,
}

/// Ask cargo. Returns a `Workspace` with `from_cargo: false` when cargo is
/// absent, the path is not a cargo project, or the manifest does not parse —
/// all of which are ordinary, and none of which should be an error, because
/// the tool must still work on a loose directory of `.rs` files.
pub fn discover(path: &Path) -> Workspace {
    let dir = if path.file_name().map(|n| n == "src").unwrap_or(false) {
        path.parent().unwrap_or(path)
    } else {
        path
    };

    let out = match std::process::Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(dir)
        .output()
    {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Workspace::default(),
    };

    let meta: Metadata = match serde_json::from_slice(&out) {
        Ok(m) => m,
        Err(_) => return Workspace::default(),
    };

    // Only workspace members. Path dependencies outside the workspace are
    // someone else's code and their dead code is not this run's business.
    let members: std::collections::HashSet<&str> =
        meta.workspace_members.iter().map(String::as_str).collect();

    let mut targets = Vec::new();
    for p in &meta.packages {
        if !members.contains(p.id.as_str()) {
            continue;
        }
        for t in &p.targets {
            targets.push(Target {
                name: t.name.clone(),
                kind: Kind::from_cargo(&t.kind),
                src_path: t.src_path.clone(),
                package: p.name.clone(),
            });
        }
    }

    // `cargo metadata` walks *up* for a manifest, so pointing the analyzer at
    // a directory that is not itself a cargo project silently answers about
    // whichever ancestor is one. Only trust the answer when it actually
    // describes the path we were asked about.
    let canon = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let describes_this_path = targets
        .iter()
        .any(|t| t.src_path.canonicalize().unwrap_or_else(|_| t.src_path.clone()).starts_with(&canon));

    if !describes_this_path {
        return Workspace::default();
    }

    let from_cargo = !targets.is_empty();
    Workspace { targets, from_cargo }
}
