//! Syntactic frontend: `syn`, no compiler, no build.
//!
//! Reads source directly, so it works on code that does not compile and costs
//! nothing but parse time. It cannot resolve types, so most symbols come back
//! `Precision::Nominal` — a bare name — and the analysis declines to judge any
//! name that is not unique.
//!
//! Everything specific to `syn` lives here. Nothing downstream imports it.

use crate::ir::*;
use proc_macro2::Span;
use quote::ToTokens;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

pub struct SynFrontend;

impl Frontend for SynFrontend {
    fn name(&self) -> &'static str {
        "syn"
    }

    fn precision(&self) -> Precision {
        Precision::Nominal
    }

    fn extract(&self, root: &Path) -> anyhow::Result<Extract> {
        let roots = resolve_roots(root);
        let mut out = Extract::default();

        for croot in &roots {
            for entry in walkdir::WalkDir::new(croot)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_file())
            {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("rs") || skipped(path) {
                    continue;
                }
                let s = path.to_string_lossy();
                let src = match std::fs::read_to_string(path) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let ast = match syn::parse_file(&src) {
                    Ok(a) => a,
                    Err(_) => continue, // unparseable: skip, do not fail the run
                };
                // Whole-file test code by Rust convention: a `tests/`
                // directory, or a file named tests.rs / test.rs / *_test.rs.
                let file_is_test = s.contains("/tests/")
                    || s.ends_with("/tests.rs")
                    || s.ends_with("/test.rs")
                    || s.ends_with("_test.rs")
                    || s.ends_with("_tests.rs");

                let mut v = FileVisitor {
                    file: path,
                    out: &mut out,
                    test_depth: usize::from(file_is_test),
                    trait_depth: 0,
                    fn_stack: Vec::new(),
                    impl_ty: None,
                    mod_stack: file_module_path(path, croot),
                    crate_root: croot.clone(),
                };
                v.visit_file(&ast);
            }
        }
        out.crate_roots = roots;
        Ok(out)
    }
}

// ─── attribute inspection ─────────────────────────────────────

/// Strip string literals from a token dump, so words inside doc comments and
/// string arguments cannot be mistaken for identifiers.
fn tokens_without_strings(a: &syn::Attribute) -> String {
    let s = a.to_token_stream().to_string();
    let mut out = String::with_capacity(s.len());
    let (mut in_str, mut prev_escape) = (false, false);
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

fn mentions_test_ident(s: &str) -> bool {
    s.split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|w| w == "test")
}

/// `#[cfg(test)]`, matched as a whole identifier with strings stripped so
/// `#[cfg(feature = "fastest")]` cannot trigger it.
fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs
        .iter()
        .any(|a| a.path().is_ident("cfg") && mentions_test_ident(&tokens_without_strings(a)))
}

/// A test harness attribute, matched by attribute *path*.
///
/// Never by substring: doc comments are attributes too, so a function
/// documented as "a test hook" — or one whose docs say "fastest" or "latest" —
/// is not a test.
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

// ─── the visitor ──────────────────────────────────────────────

struct FileVisitor<'a> {
    file: &'a Path,
    out: &'a mut Extract,
    /// Depth of nested `#[cfg(test)]` modules.
    test_depth: usize,
    /// Depth of nested `impl Trait for Type` blocks.
    trait_depth: usize,
    /// Self type of the innermost `impl` block, when it is a plain path.
    impl_ty: Option<String>,
    /// Enclosing `mod` names, outermost first, including the module this file
    /// itself is. Lets a definition be told apart from a same-named one
    /// elsewhere in the crate.
    mod_stack: Vec<String>,
    /// Enclosing function names, innermost last. A call recorded while this is
    /// non-empty becomes a graph edge from its innermost entry.
    fn_stack: Vec<String>,
    crate_root: PathBuf,
}

impl<'a> FileVisitor<'a> {
    fn in_test(&self) -> bool {
        self.test_depth > 0
    }

    fn caller(&self) -> SymbolId {
        SymbolId::nominal(self.fn_stack.last().cloned().unwrap_or_default())
    }

    fn record_def(&mut self, name: String, span: Span, attrs: &[syn::Attribute], is_pub: bool) {
        let is_ffi = attrs.iter().any(|a| {
            let t = a.to_token_stream().to_string();
            a.path().is_ident("no_mangle") || t.contains("export_name") || t.contains("used")
        });
        self.out.definitions.push(Definition {
            id: SymbolId::nominal(name),
            precision: Precision::Nominal,
            file: self.file.to_path_buf(),
            line: line_of(span),
            in_test: self.in_test() || has_cfg_test(attrs),
            trait_impl: self.trait_depth > 0,
            allowed_dead: has_allow_dead(attrs),
            crate_root: self.crate_root.clone(),
            is_pub,
            is_test_fn: attrs.iter().any(is_test_attr),
            is_ffi,
            self_ty: self.impl_ty.clone(),
            module: (!self.mod_stack.is_empty()).then(|| self.mod_stack.join("::")),
        });
    }

    fn record_edge(&mut self, to: String, span: Span, kind: EdgeKind) {
        let from = self.caller();
        self.out.edges.push(Edge {
            from,
            to: SymbolId::nominal(to),
            kind,
            precision: Precision::Nominal,
            in_test: self.in_test(),
            file: self.file.to_path_buf(),
            line: line_of(span),
        });
    }

    /// Walk a macro's token stream, recording `ident (` as a call.
    ///
    /// Only from production context, and tagged `MacroToken` so the analysis
    /// knows it may suppress a finding but never create one: token matching
    /// cannot tell a call from a tuple-struct literal, and crediting a
    /// spurious *test* call would push a function with no real callers into
    /// the report.
    fn record_tokens(&mut self, ts: proc_macro2::TokenStream) {
        use proc_macro2::TokenTree;
        let mut prev: Option<proc_macro2::Ident> = None;
        for tt in ts {
            match tt {
                TokenTree::Ident(id) => prev = Some(id),
                TokenTree::Group(g) => {
                    if g.delimiter() == proc_macro2::Delimiter::Parenthesis {
                        if let Some(id) = prev.take() {
                            if !self.in_test() {
                                let span = id.span();
                                self.record_edge(id.to_string(), span, EdgeKind::MacroToken);
                            }
                        }
                    }
                    // Nested groups hold the macro's real body.
                    self.record_tokens(g.stream());
                    prev = None;
                }
                _ => prev = None,
            }
        }
    }
}

impl<'ast, 'a> Visit<'ast> for FileVisitor<'a> {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        let is_test = has_cfg_test(&node.attrs);
        if is_test {
            self.test_depth += 1;
        }
        self.mod_stack.push(node.ident.to_string());
        syn::visit::visit_item_mod(self, node);
        self.mod_stack.pop();
        if is_test {
            self.test_depth -= 1;
        }
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        let is_trait = node.trait_.is_some();
        if is_trait {
            self.trait_depth += 1;
        }
        // Remember the self type so a definition can carry it as metadata.
        // Only a plain path: a generic or reference receiver is not a name a
        // call site could be matched against.
        let prev = self.impl_ty.take();
        if let syn::Type::Path(p) = &*node.self_ty {
            self.impl_ty = p.path.segments.last().map(|s| s.ident.to_string());
        }
        syn::visit::visit_item_impl(self, node);
        self.impl_ty = prev;
        if is_trait {
            self.trait_depth -= 1;
        }
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        // A #[test] fn is test code even outside a cfg(test) module.
        let is_test_fn = node.attrs.iter().any(is_test_attr) || has_cfg_test(&node.attrs);
        let is_pub = matches!(node.vis, syn::Visibility::Public(_));
        let name = node.sig.ident.to_string();
        self.record_def(name.clone(), node.sig.ident.span(), &node.attrs, is_pub);
        self.fn_stack.push(name);
        if is_test_fn {
            self.test_depth += 1;
            syn::visit::visit_item_fn(self, node);
            self.test_depth -= 1;
        } else {
            syn::visit::visit_item_fn(self, node);
        }
        self.fn_stack.pop();
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        let is_pub = matches!(node.vis, syn::Visibility::Public(_));
        let name = node.sig.ident.to_string();
        self.record_def(name.clone(), node.sig.ident.span(), &node.attrs, is_pub);
        self.fn_stack.push(name);
        syn::visit::visit_impl_item_fn(self, node);
        self.fn_stack.pop();
    }

    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
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
            collect(&node.tree, &mut self.out.reexported);
        }
        syn::visit::visit_item_use(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        // A macro body is an opaque token stream to syn, so calls written
        // inside one are invisible to the expression visitors. Codebases that
        // generate whole subsystems through macros would otherwise look as
        // though nothing calls them.
        self.record_tokens(node.tokens.clone());
        syn::visit::visit_macro(self, node);
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(p) = &*node.func {
            if let Some(seg) = p.path.segments.last() {
                let span = seg.ident.span();
                self.record_edge(seg.ident.to_string(), span, EdgeKind::Call);
            }
        }
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let span = node.method.span();
        self.record_edge(node.method.to_string(), span, EdgeKind::MethodCall);
        syn::visit::visit_expr_method_call(self, node);
    }
}

/// The module path a file defines, from its position under the crate root.
/// `src/alpha.rs` -> `["alpha"]`, `src/a/b.rs` -> `["a", "b"]`, and the crate
/// roots `lib.rs` / `main.rs` -> `[]`.
fn file_module_path(file: &Path, croot: &Path) -> Vec<String> {
    let rel = match file.strip_prefix(croot) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut parts: Vec<String> = rel
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(str::to_string))
        .collect();
    let last = parts.pop().unwrap_or_default();
    let stem = last.trim_end_matches(".rs");
    if !matches!(stem, "lib" | "main" | "mod") {
        parts.push(stem.to_string());
    }
    parts
}

// ─── file discovery ───────────────────────────────────────────

/// Directory segments holding copies of code the crate does not own.
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

/// Which directories to read.
///
/// Cargo knows exactly which files it compiles, including targets declared in
/// the manifest that live nowhere the convention would predict. Where it
/// cannot answer, fall back to directory shape — the tool must still work on
/// a loose directory of `.rs` files.
pub fn resolve_roots(path: &Path) -> Vec<PathBuf> {
    let ws = crate::targets::discover(path);
    if ws.from_cargo {
        let dirs = ws.production_source_dirs();
        if !dirs.is_empty() {
            return dirs;
        }
    }

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
