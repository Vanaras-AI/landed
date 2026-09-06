//! Languages the analyzer can read, and how to tell them apart.
//!
//! Nothing in the analysis layer refers to a language. This module and the
//! frontends are the only places that know one exists.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Language {
    Rust,
    Python,
    TypeScript,
    Go,
}

impl Language {
    pub fn name(&self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Python => "python",
            Language::TypeScript => "typescript",
            Language::Go => "go",
        }
    }

    /// File extensions this language owns.
    pub fn extensions(&self) -> &'static [&'static str] {
        match self {
            Language::Rust => &["rs"],
            Language::Python => &["py"],
            Language::TypeScript => &["ts", "tsx", "mts", "cts"],
            Language::Go => &["go"],
        }
    }

    pub fn owns(&self, p: &Path) -> bool {
        p.extension()
            .and_then(|e| e.to_str())
            .map(|e| self.extensions().contains(&e))
            .unwrap_or(false)
    }

    /// Manifest files that identify a project of this language, most specific
    /// first. Presence of one is stronger evidence than a file count.
    pub fn manifests(&self) -> &'static [&'static str] {
        match self {
            Language::Rust => &["Cargo.toml"],
            Language::Python => &["pyproject.toml", "setup.py", "setup.cfg"],
            Language::TypeScript => &["tsconfig.json", "package.json"],
            Language::Go => &["go.mod"],
        }
    }

    pub fn all() -> &'static [Language] {
        &[
            Language::Rust,
            Language::Python,
            Language::TypeScript,
            Language::Go,
        ]
    }
}

impl std::str::FromStr for Language {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "rust" | "rs" => Ok(Language::Rust),
            "python" | "py" => Ok(Language::Python),
            "typescript" | "ts" => Ok(Language::TypeScript),
            "go" | "golang" => Ok(Language::Go),
            other => Err(format!(
                "unknown language {other:?}; known: rust, python, typescript, go"
            )),
        }
    }
}

/// Work out which language a directory holds.
///
/// A manifest decides it outright. Failing that, the extension with the most
/// files wins — a project can contain a stray script of another language
/// without changing what it is.
///
/// Returns `None` when nothing recognisable is present, which is a real
/// answer: the caller should say so rather than guess and analyse nothing.
pub fn detect(root: &Path) -> Option<Language> {
    for lang in Language::all() {
        for m in lang.manifests() {
            if root.join(m).is_file() {
                return Some(*lang);
            }
        }
    }
    // A `src/` directory handed to us directly: check its parent too.
    if let Some(parent) = root.parent() {
        for lang in Language::all() {
            for m in lang.manifests() {
                if parent.join(m).is_file() {
                    return Some(*lang);
                }
            }
        }
    }

    let mut counts: std::collections::HashMap<Language, usize> = std::collections::HashMap::new();
    for e in walkdir::WalkDir::new(root)
        .max_depth(6)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        for lang in Language::all() {
            if lang.owns(e.path()) {
                *counts.entry(*lang).or_default() += 1;
            }
        }
    }
    // Ties broken by declaration order, so the answer does not depend on hash
    // iteration.
    Language::all()
        .iter()
        .filter_map(|l| counts.get(l).map(|c| (*c, *l)))
        .max_by_key(|(c, l)| (*c, std::cmp::Reverse(*l)))
        .map(|(_, l)| l)
}
