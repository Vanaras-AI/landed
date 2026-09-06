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

    /// Files a host loads and calls into, rather than files this project's own
    /// code calls. Whatever they export is invoked from outside and is a root.
    ///
    /// A Rust binary needs none of this: its entry is a function called
    /// `main`, and a name is enough. An editor extension, a serverless
    /// handler and a plugin are all entered by something that is not in the
    /// repository, at a name only the entry module knows. Without this, the
    /// entry function looks uncalled and everything behind it is condemned —
    /// one broken root turned 107 live functions into findings before this
    /// existed.
    fn entry_files(&self) -> Vec<PathBuf> {
        Vec::new()
    }

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
        Self {
            root: root.to_path_buf(),
            workspace: crate::targets::discover(root),
        }
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
        let mut v: Vec<PathBuf> = dirs.iter().flat_map(|d| walk(d, Language::Rust)).collect();
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
                if self.is_application() {
                    "application"
                } else {
                    "library"
                }
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
        Self {
            root: root.to_path_buf(),
            lang,
        }
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
            Language::Python => in_test_dir || stem.starts_with("test_") || stem.ends_with("_test"),
            // jest, vitest and mocha all use these.
            Language::TypeScript => {
                in_test_dir
                    || stem.ends_with(".test")
                    || stem.ends_with(".spec")
                    || stem.ends_with("_test")
            }
            // The go tool defines this one exactly — and then projects put
            // shared test scaffolding in ordinary `.go` files so that other
            // packages' tests can import it, which the naming rule alone
            // reads as production code.
            //
            // Importing the standard `testing` package settles it. That
            // package only functions inside a test binary — it registers
            // flags and expects the test harness — so a file importing it is
            // test support whatever it is called. It is also a rare thing to
            // do: 5 files out of 521 on the project this was found on, so it
            // discriminates rather than sweeps.
            //
            // Matching the *import*, not the word, deliberately. An earlier
            // version of this analysis matched "test" as a substring and read
            // "fastest" and "is a test hook" as test markers.
            Language::Go => {
                stem.ends_with("_test")
                    || std::fs::read_to_string(p)
                        .map(|src| {
                            src.lines().any(|l| {
                                let l = l.trim();
                                l == "\"testing\""
                                    || l.starts_with("\"testing\" ")
                                    || l == "import \"testing\""
                                    || l.starts_with("_ \"testing\"")
                            })
                        })
                        .unwrap_or(false)
            }
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
            // Is this something people run, or something people import?
            //
            // A `__main__.py` and a console script say only that it *can* be
            // run, and almost every serious library on PyPI can be: a web
            // framework ships a dev server, a formatter ships a command. What
            // makes a project a library is being packaged for other people's
            // code to import — which `[project]` in pyproject.toml, or a
            // setup.py, is exactly the declaration of.
            //
            // Reading only the runnable signals classified the best-known
            // Python web framework as an application. That stops its public
            // API being a root, and it then reported that framework's
            // most-used function, and 39 functions behind it, as confidently
            // dead. A tool that does this twice is never run again.
            //
            // So packaging decides: a project that declares a distribution is
            // a library even when it also has a command, and an application
            // is a repository you run and nobody installs.
            Language::Python => {
                let packaged = self.root.join("setup.py").is_file()
                    || self.root.join("setup.cfg").is_file()
                    || std::fs::read_to_string(self.root.join("pyproject.toml"))
                        .map(|s| s.contains("[project]") || s.contains("[tool.poetry]"))
                        .unwrap_or(false);
                !packaged
                    && self.source_files().iter().any(|p| {
                        p.file_name().map(|f| f == "__main__.py").unwrap_or(false)
                            || std::fs::read_to_string(p)
                                .map(|s| s.contains("__main__"))
                                .unwrap_or(false)
                    })
            }
            // Ordered, because the signals overlap and the wrong order
            // gets the common cases backwards.
            //
            // Two earlier signals were simply wrong. `"engines"` was read as
            // "a host loads this", but almost every published library states
            // `{"node": ">=20"}` there — a compatibility range, not a host.
            // And `"private": true` was read as "not published", when on a
            // monorepo root it means only that the *root* is not published
            // while every package under it is. Between them they classified a
            // widely-used state library as an application and reported half
            // its functions dead.
            //
            // What survives is the same rule Python needed: a package that
            // declares an import surface is a library, even when it also
            // ships a command.
            Language::TypeScript => {
                let manifest =
                    std::fs::read_to_string(self.root.join("package.json")).unwrap_or_default();
                let json: serde_json::Value =
                    serde_json::from_str(&manifest).unwrap_or(serde_json::Value::Null);
                let has = |k: &str| json.get(k).is_some();

                // 1. A host other than the runtime itself loads this and calls
                //    into it — an editor extension, say. `node` and the
                //    package managers are version constraints, not hosts.
                let host_loaded = json
                    .get("engines")
                    .and_then(|e| e.as_object())
                    .map(|o| {
                        o.keys()
                            .any(|k| !matches!(k.as_str(), "node" | "npm" | "yarn" | "pnpm"))
                    })
                    .unwrap_or(false);
                if host_loaded {
                    return true;
                }

                // 2. A bundler entry point beside the manifest: a web app.
                if self.root.join("index.html").is_file() {
                    return true;
                }

                // 3. Anything importable is a library, CLI or not.
                if has("main") || has("module") || has("exports") || has("types") {
                    return false;
                }

                // 4. A command and nothing to import: an application.
                //    And a package declaring neither is something you run.
                true
            }
            Language::Rust => self.root.join("src/main.rs").is_file(),
        }
    }

    fn entry_files(&self) -> Vec<PathBuf> {
        if self.lang != Language::TypeScript || !self.is_application() {
            return Vec::new();
        }
        let manifest = match std::fs::read_to_string(self.root.join("package.json")) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let json: serde_json::Value = match serde_json::from_str(&manifest) {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };

        let mut out = Vec::new();
        for key in ["main", "module"] {
            let Some(decl) = json.get(key).and_then(|v| v.as_str()) else {
                continue;
            };
            // The manifest names the *built* file. Sources are what is
            // analysed, so the declaration is mapped back: the build
            // directory becomes a source directory, and the extension becomes
            // one TypeScript writes.
            let rel = decl.trim_start_matches("./");
            let stem = rel.rsplit_once('.').map(|(a, _)| a).unwrap_or(rel);
            let tail = stem.split_once('/').map(|(head, rest)| {
                if matches!(head, "out" | "dist" | "build" | "lib") {
                    rest
                } else {
                    stem
                }
            });
            for candidate in [tail.unwrap_or(stem), stem] {
                for dir in ["src", ""] {
                    for ext in ["ts", "tsx"] {
                        let p = if dir.is_empty() {
                            self.root.join(format!("{candidate}.{ext}"))
                        } else {
                            self.root.join(dir).join(format!("{candidate}.{ext}"))
                        };
                        if p.is_file() && !out.contains(&p) {
                            out.push(p);
                        }
                    }
                }
            }
        }
        out
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
            if self.is_application() {
                "application"
            } else {
                "library"
            }
        )
    }
}
