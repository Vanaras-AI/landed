//! What a project is, independent of the language it is written in.
//!
//! Reachability needs three things from a project and nothing else: which
//! files hold production code, which hold tests, and where execution starts.
//! Every language answers differently — `[[bin]]` and `#[cfg(test)]`,
//! `if __name__ == "__main__"` and `test_*.py`, `"bin"` in package.json — but
//! the analysis only ever asks the question, never the language.
//!
//! This is the boundary that was previously hard-wired to cargo.

use crate::lang::Language;
use std::path::{Path, PathBuf};

/// Directory segments holding copies of code the project does not own.
pub const SKIP_DIRS: &[&str] = &[
    "/target/",
    "/node_modules/",
    "/.git/",
    "/worktrees/",
    "/vendor/",
    "/.cargo/",
    "/build/",
    "/dist/",
    "/temp/",
    "/examples/",
    "/.venv/",
    "/venv/",
    "/site-packages/",
    "/__pycache__/",
    "/.tox/",
    "/.mypy_cache/",
];

pub fn skipped(p: &Path) -> bool {
    let s = format!("{}/", p.to_string_lossy());
    SKIP_DIRS.iter().any(|d| s.contains(d))
}

pub trait Project {
    fn language(&self) -> Language;

    /// Every file to read, production and test alike. Test files are still
    /// read — a call from a test is what makes a finding a finding rather
    /// than simply unreferenced code.
    fn source_files(&self) -> Vec<PathBuf>;

    /// Is this file wholly test code, by the language's own convention?
    fn is_test_file(&self, path: &Path) -> bool;

    /// Does something here run on its own? An application's internal library
    /// surface is not an entry point; a library's is all it has.
    fn is_application(&self) -> bool;

    /// Names always reachable regardless of who calls them — a language's
    /// conventional entry points.
    fn entry_points(&self) -> Vec<String>;

    /// Human-readable account of how those answers were reached, for
    /// `--explain` and for anyone who thinks the analysis picked wrong.
    fn describe(&self) -> String;
}

/// Build the right project for a path.
pub fn detect(root: &Path) -> Box<dyn Project> {
    detect_as(root, None)
}

/// As `detect`, with the language stated rather than inferred.
pub fn detect_as(root: &Path, lang: Option<Language>) -> Box<dyn Project> {
    match lang.or_else(|| crate::lang::detect(root)) {
        Some(Language::Rust) => Box::new(CargoProject::new(root)),
        Some(lang) => Box::new(ConventionProject::new(root, lang)),
        // Nothing recognisable: read what is there and assume the least.
        None => Box::new(ConventionProject::new(root, Language::Rust)),
    }
}

fn walk(root: &Path, lang: Language) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !skipped(e.path()))
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| lang.owns(p))
        .collect();
    v.sort();
    v
}

// ─── Rust, via cargo ──────────────────────────────────────────

pub struct CargoProject {
    root: PathBuf,
    workspace: crate::targets::Workspace,
}

impl CargoProject {
    pub fn new(root: &Path) -> Self {
        Self { root: root.to_path_buf(), workspace: crate::targets::discover(root) }
    }

    pub fn workspace(&self) -> &crate::targets::Workspace {
        &self.workspace
    }
}

impl Project for CargoProject {
    fn language(&self) -> Language {
        Language::Rust
    }

    fn source_files(&self) -> Vec<PathBuf> {
        // Cargo knows exactly which files it compiles, including targets
        // declared in the manifest that live nowhere the convention predicts.
        let dirs = if self.workspace.from_cargo {
            let d = self.workspace.production_source_dirs();
            if d.is_empty() {
                crate::frontend::syn_frontend::resolve_roots(&self.root)
            } else {
                d
            }
        } else {
            crate::frontend::syn_frontend::resolve_roots(&self.root)
        };
        let mut v: Vec<PathBuf> =
            dirs.iter().flat_map(|d| walk(d, Language::Rust)).collect();
        v.sort();
        v.dedup();
        v
    }

    fn is_test_file(&self, p: &Path) -> bool {
        let s = p.to_string_lossy();
        s.contains("/tests/")
            || s.ends_with("/tests.rs")
            || s.ends_with("/test.rs")
            || s.ends_with("_test.rs")
            || s.ends_with("_tests.rs")
    }

    fn is_application(&self) -> bool {
        if self.workspace.from_cargo {
            self.workspace.is_application()
        } else {
            crate::frontend::syn_frontend::resolve_roots(&self.root)
                .iter()
                .any(|r| r.join("main.rs").is_file() || r.join("bin").is_dir())
        }
    }

    fn entry_points(&self) -> Vec<String> {
        // Every cargo binary target, however it is named in the manifest,
        // starts at a function called `main`. The target's own name is not a
        // function name, and treating it as one makes any same-named function
        // — a test helper, say — a production entry point.
        vec!["main".to_string()]
    }

    fn describe(&self) -> String {
        if self.workspace.from_cargo {
            format!(
                "rust, {} cargo target(s); {}",
                self.workspace.targets.len(),
                if self.is_application() { "application" } else { "library" }
            )
        } else {
            "rust, cargo could not answer; directory conventions used".into()
        }
    }
}

// ─── Everything else, by convention ───────────────────────────

/// A project identified by its manifests and layout rather than by asking a
/// build tool. Enough for languages whose conventions are strong, which is
/// most of them.
pub struct ConventionProject {
    root: PathBuf,
    lang: Language,
}

impl ConventionProject {
    pub fn new(root: &Path, lang: Language) -> Self {
        Self { root: root.to_path_buf(), lang }
    }
}

impl Project for ConventionProject {
    fn language(&self) -> Language {
        self.lang
    }

    fn source_files(&self) -> Vec<PathBuf> {
        walk(&self.root, self.lang)
    }

    fn is_test_file(&self, p: &Path) -> bool {
        let s = p.to_string_lossy();
        let stem = p.file_stem().and_then(|x| x.to_str()).unwrap_or("");
        let in_test_dir = s.contains("/tests/")
            || s.contains("/test/")
            || s.contains("/__tests__/")
            || s.contains("/spec/");
        match self.lang {
            // pytest and unittest both key on the filename.
            Language::Python => {
                in_test_dir || stem.starts_with("test_") || stem.ends_with("_test")
            }
            // jest, vitest and mocha all use these.
            Language::TypeScript => {
                in_test_dir
                    || stem.ends_with(".test")
                    || stem.ends_with(".spec")
                    || stem.ends_with("_test")
            }
            // The go tool defines this one exactly.
            Language::Go => stem.ends_with("_test"),
            Language::Rust => {
                in_test_dir || stem == "tests" || stem == "test" || stem.ends_with("_test")
            }
        }
    }

    fn is_application(&self) -> bool {
        match self.lang {
            // A `func main` in `package main` is the definition.
            Language::Go => self
                .source_files()
                .iter()
                .filter(|p| !self.is_test_file(p))
                .any(|p| {
                    std::fs::read_to_string(p)
                        .map(|s| s.contains("package main"))
                        .unwrap_or(false)
                }),
            // A module guarded by `if __name__ == "__main__"` is runnable; so
            // is a declared console script, and so is a package holding a
            // `__main__.py`, which `python -m` runs on the strength of its
            // name alone — nothing need be written inside it.
            Language::Python => {
                let declares_script = std::fs::read_to_string(self.root.join("pyproject.toml"))
                    .map(|s| s.contains("[project.scripts]") || s.contains("console_scripts"))
                    .unwrap_or(false);
                declares_script
                    || self.source_files().iter().any(|p| {
                        p.file_name().map(|f| f == "__main__.py").unwrap_or(false)
                            || std::fs::read_to_string(p)
                                .map(|s| s.contains("__main__"))
                                .unwrap_or(false)
                    })
            }
            Language::TypeScript => std::fs::read_to_string(self.root.join("package.json"))
                .map(|s| s.contains("\"bin\""))
                .unwrap_or(false),
            Language::Rust => self.root.join("src/main.rs").is_file(),
        }
    }

    fn entry_points(&self) -> Vec<String> {
        match self.lang {
            Language::Go => vec!["main".into(), "init".into()],
            Language::Python => vec!["main".into()],
            Language::TypeScript => vec!["main".into()],
            Language::Rust => vec!["main".into()],
        }
    }

    fn describe(&self) -> String {
        format!(
            "{}, by convention; {}",
            self.lang.name(),
            if self.is_application() { "application" } else { "library" }
        )
    }
}
