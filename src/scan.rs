//! AST scan: collect function definitions and call sites, classified by
//! whether they live in production code or test code.

use proc_macro2::Span;
use quote::ToTokens;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

#[derive(Debug, Clone, serde::Serialize)]
pub struct FnDef {
    pub name: String,
    pub file: PathBuf,
    pub line: usize,
    /// Defined inside a `#[cfg(test)]` module.
    pub in_test: bool,
    /// Method of a trait impl (`impl Trait for Type`) — reachable via dynamic
    /// dispatch, so absence of a direct call site proves nothing.
    pub trait_impl: bool,
    /// Carries `#[allow(dead_code)]` — author already acknowledged this.
    pub allowed_dead: bool,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct CallSites {
    pub prod: usize,
    pub test: usize,
    /// Up to 3 example locations, for the report.
    pub examples: Vec<String>,
}

#[derive(Default)]
pub struct Scan {
    pub defs: Vec<FnDef>,
    pub calls: HashMap<String, CallSites>,
    /// Names re-exported from the crate root (`pub use ...`). These are the
    /// crate's public API: consumers, benches and fuzz targets live outside
    /// the tree we scan, so "no in-crate caller" proves nothing about them.
    pub reexported: std::collections::HashSet<String>,
}

/// Strip string literals from a token dump, so words inside doc comments and
/// string arguments cannot be mistaken for identifiers.
fn tokens_without_strings(a: &syn::Attribute) -> String {
    let s = a.to_token_stream().to_string();
    let mut out = String::with_capacity(s.len());
    let mut in_str = false;
    let mut prev_escape = false;
    for c in s.chars() {
        match c {
            '"' if !prev_escape => in_str = !in_str,
            _ if !in_str => out.push(c),
            _ => {}
        }
        prev_escape = c == '\\' && !prev_escape;
    }
    out
}

/// Is `test` present as a bare identifier (not inside a string)?
fn mentions_test_ident(s: &str) -> bool {
    s.split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|w| w == "test")
}

/// Does this attribute list contain `#[cfg(test)]`?
///
/// Must not match `#[cfg(feature = "fastest")]`, so string literals are
/// stripped and `test` is matched as a whole identifier.
fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs
        .iter()
        .any(|a| a.path().is_ident("cfg") && mentions_test_ident(&tokens_without_strings(a)))
}

/// Is this a test harness attribute — `#[test]`, `#[tokio::test]`, `#[bench]`,
/// `#[rstest]`, `#[proptest]`?
///
/// Checked by attribute *path*, never by substring: doc comments are
/// attributes too, so a function documented as "a test hook" is not a test.
fn is_test_attr(a: &syn::Attribute) -> bool {
    let last = match a.path().segments.last() {
        Some(s) => s.ident.to_string(),
        None => return false,
    };
    matches!(
        last.as_str(),
        "test" | "bench" | "rstest" | "proptest" | "quickcheck" | "test_case"
    )
}

fn has_allow_dead(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        let s = a.to_token_stream().to_string();
        a.path().is_ident("allow") && (s.contains("dead_code") || s.contains("unused"))
    })
}

fn line_of(span: Span) -> usize {
    span.start().line
}

struct FileVisitor<'a> {
    file: &'a Path,
    scan: &'a mut Scan,
    /// Depth of nested `#[cfg(test)]` modules we are currently inside.
    test_depth: usize,
    /// Depth of nested `impl Trait for Type` blocks.
    trait_depth: usize,
}

impl<'a> FileVisitor<'a> {
    fn in_test(&self) -> bool {
        self.test_depth > 0
    }

    fn record_def(&mut self, name: String, span: Span, attrs: &[syn::Attribute]) {
        self.scan.defs.push(FnDef {
            name,
            file: self.file.to_path_buf(),
            line: line_of(span),
            in_test: self.in_test() || has_cfg_test(attrs),
            trait_impl: self.trait_depth > 0,
            allowed_dead: has_allow_dead(attrs),
        });
    }

    fn record_call(&mut self, name: String, span: Span) {
        let in_test = self.in_test();
        let file = self.file.to_path_buf();
        let entry = self.scan.calls.entry(name).or_default();
        if in_test {
            entry.test += 1;
            if entry.examples.len() < 3 {
                entry
                    .examples
                    .push(format!("{}:{}", file.display(), line_of(span)));
            }
        } else {
            entry.prod += 1;
        }
    }
}

impl<'ast, 'a> Visit<'ast> for FileVisitor<'a> {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        let is_test = has_cfg_test(&node.attrs);
        if is_test {
            self.test_depth += 1;
        }
        syn::visit::visit_item_mod(self, node);
        if is_test {
            self.test_depth -= 1;
        }
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        let is_trait = node.trait_.is_some();
        if is_trait {
            self.trait_depth += 1;
        }
        syn::visit::visit_item_impl(self, node);
        if is_trait {
            self.trait_depth -= 1;
        }
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        // A #[test] fn is test code even outside a cfg(test) module.
        let is_test_fn = node.attrs.iter().any(is_test_attr) || has_cfg_test(&node.attrs);
        self.record_def(node.sig.ident.to_string(), node.sig.ident.span(), &node.attrs);
        if is_test_fn {
            self.test_depth += 1;
            syn::visit::visit_item_fn(self, node);
            self.test_depth -= 1;
        } else {
            syn::visit::visit_item_fn(self, node);
        }
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.record_def(node.sig.ident.to_string(), node.sig.ident.span(), &node.attrs);
        syn::visit::visit_impl_item_fn(self, node);
    }

    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        // Record every name in a `pub use ...` as crate public API.
        if matches!(node.vis, syn::Visibility::Public(_)) {
            fn collect(tree: &syn::UseTree, out: &mut std::collections::HashSet<String>) {
                match tree {
                    syn::UseTree::Name(n) => {
                        out.insert(n.ident.to_string());
                    }
                    syn::UseTree::Rename(r) => {
                        out.insert(r.rename.to_string());
                        out.insert(r.ident.to_string());
                    }
                    syn::UseTree::Path(p) => collect(&p.tree, out),
                    syn::UseTree::Group(g) => g.items.iter().for_each(|t| collect(t, out)),
                    syn::UseTree::Glob(_) => {}
                }
            }
            collect(&node.tree, &mut self.scan.reexported);
        }
        syn::visit::visit_item_use(self, node);
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(p) = &*node.func {
            if let Some(seg) = p.path.segments.last() {
                self.record_call(seg.ident.to_string(), seg.ident.span());
            }
        }
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        self.record_call(node.method.to_string(), node.method.span());
        syn::visit::visit_expr_method_call(self, node);
    }
}

/// Directory segments that hold copies of code the crate does not own.
const SKIP_DIRS: &[&str] = &[
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
];

fn skipped(p: &Path) -> bool {
    let s = format!("{}/", p.to_string_lossy());
    SKIP_DIRS.iter().any(|d| s.contains(d))
}

/// Resolve what to actually scan.
///
/// A Rust crate is a directory with a `Cargo.toml`; its code lives in `src/`.
/// A workspace holds several such crates. Scanning a whole repo tree sweeps in
/// vendored copies and unrelated nested projects, so instead we locate every
/// crate the repo owns and scan each one's `src/`.
pub fn resolve_roots(path: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for entry in walkdir::WalkDir::new(path)
        .max_depth(4)
        .into_iter()
        .filter_entry(|e| !skipped(e.path()))
        .filter_map(Result::ok)
    {
        if entry.file_name() == "Cargo.toml" {
            let src = entry.path().with_file_name("src");
            if src.is_dir() {
                roots.push(src);
            }
        }
    }
    if roots.is_empty() {
        roots.push(path.to_path_buf());
    }
    roots
}

/// Walk a crate (or workspace), parse every `.rs` file, collect defs + calls.
pub fn scan_crate(root: &Path) -> anyhow::Result<Scan> {
    let roots = resolve_roots(root);
    let mut scan = Scan::default();
    for entry in roots
        .iter()
        .flat_map(|r| walkdir::WalkDir::new(r).into_iter())
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        if skipped(path) {
            continue;
        }
        let s = path.to_string_lossy();
        let src = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let ast = match syn::parse_file(&src) {
            Ok(a) => a,
            Err(_) => continue, // unparseable file: skip, don't fail the run
        };
        // Whole-file test code, by Rust convention: a `tests/` directory, or a
        // file named `tests.rs` / `test.rs` / `*_test.rs` / `*_tests.rs`.
        // Missing `src/**/tests.rs` treats every test helper in it as shipped
        // production code, which is how you manufacture false positives.
        let file_is_test = s.contains("/tests/")
            || s.ends_with("/tests.rs")
            || s.ends_with("/test.rs")
            || s.ends_with("_test.rs")
            || s.ends_with("_tests.rs");
        let mut v = FileVisitor {
            file: path,
            scan: &mut scan,
            test_depth: usize::from(file_is_test),
            trait_depth: 0,
        };
        v.visit_file(&ast);
    }
    Ok(scan)
}

/// Names that are always reachable or conventionally unreferenced.
const ALWAYS_LIVE: &[&str] = &[
    "main", "new", "default", "fmt", "from", "from_str", "try_from", "drop", "clone",
    "next", "poll", "deref", "deref_mut", "eq", "ne", "hash", "cmp", "partial_cmp",
    "serialize", "deserialize", "into", "as_ref", "borrow",
];

#[derive(Debug, serde::Serialize)]
pub struct Finding {
    pub name: String,
    pub file: String,
    pub line: usize,
    pub test_calls: usize,
    pub examples: Vec<String>,
}

/// A production fn whose only callers are tests: it shipped, but nothing runs it.
pub fn never_run(scan: &Scan) -> Vec<Finding> {
    let mut out = Vec::new();
    for d in &scan.defs {
        if d.in_test || d.trait_impl || d.allowed_dead {
            continue;
        }
        if ALWAYS_LIVE.contains(&d.name.as_str()) {
            continue;
        }
        // Re-exported from the crate root: it is public API, and its consumers
        // are outside this tree.
        if scan.reexported.contains(&d.name) {
            continue;
        }
        // A name defined more than once is ambiguous under name-based matching;
        // skip it rather than risk a false positive.
        if scan.defs.iter().filter(|o| o.name == d.name && !o.in_test).count() > 1 {
            continue;
        }
        if let Some(c) = scan.calls.get(&d.name) {
            if c.prod == 0 && c.test > 0 {
                out.push(Finding {
                    name: d.name.clone(),
                    file: d.file.display().to_string(),
                    line: d.line,
                    test_calls: c.test,
                    examples: c.examples.clone(),
                });
            }
        }
    }
    out.sort_by(|a, b| b.test_calls.cmp(&a.test_calls));
    out
}
