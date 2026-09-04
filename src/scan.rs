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
    /// `pub` at its definition site. In a library crate the public surface is
    /// an entry point: callers live outside the tree we can see.
    pub is_pub: bool,
    /// A test harness function (`#[test]`, `#[bench]`, …) — a root of the
    /// test-reachable set, never of the production one.
    pub is_test_fn: bool,
    /// `#[no_mangle]` / `extern "C"` — callable from assembly or another
    /// language, so it is an entry point no matter who calls it in Rust.
    pub is_ffi: bool,
    /// The crate `src/` directory this definition came from. Entry points are
    /// a property of a crate, not of a workspace, so each definition has to
    /// remember which crate it belongs to.
    pub crate_root: PathBuf,
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
    /// The call graph: caller name -> names it invokes. Built alongside
    /// `calls` so reachability can be computed transitively from entry
    /// points, rather than asking each function in isolation whether anyone
    /// mentions it. Calls made outside any function (a `static` initialiser,
    /// a const expression) are attributed to the caller `""`.
    pub edges: HashMap<String, std::collections::HashSet<String>>,
    /// Every crate `src/` dir that was scanned.
    pub crate_roots: Vec<PathBuf>,
    /// Crate layout as cargo reports it. Empty when cargo could not answer,
    /// in which case directory heuristics apply.
    pub workspace: crate::targets::Workspace,
    /// Project configuration: developer-declared roots and ignores.
    pub config: crate::config::Config,
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
    /// Enclosing function names, innermost last. A call recorded while this
    /// is non-empty becomes a graph edge from its innermost entry.
    fn_stack: Vec<String>,
    /// The crate `src/` this file belongs to.
    crate_root: PathBuf,
}

impl<'a> FileVisitor<'a> {
    fn in_test(&self) -> bool {
        self.test_depth > 0
    }

    fn record_def(&mut self, name: String, span: Span, attrs: &[syn::Attribute], is_pub: bool) {
        let is_ffi = attrs.iter().any(|a| {
            let t = a.to_token_stream().to_string();
            a.path().is_ident("no_mangle") || t.contains("export_name") || t.contains("used")
        });
        self.scan.defs.push(FnDef {
            name,
            file: self.file.to_path_buf(),
            line: line_of(span),
            in_test: self.in_test() || has_cfg_test(attrs),
            trait_impl: self.trait_depth > 0,
            allowed_dead: has_allow_dead(attrs),
            crate_root: self.crate_root.clone(),
            is_pub,
            is_test_fn: attrs.iter().any(is_test_attr),
            is_ffi,
        });
    }

    /// Walk a macro's token stream, recording `ident (` as a call.
    fn record_tokens(&mut self, ts: proc_macro2::TokenStream) {
        use proc_macro2::TokenTree;
        let mut prev: Option<proc_macro2::Ident> = None;
        for tt in ts {
            match tt {
                TokenTree::Ident(id) => {
                    prev = Some(id);
                }
                TokenTree::Group(g) => {
                    if g.delimiter() == proc_macro2::Delimiter::Parenthesis {
                        if let Some(id) = prev.take() {
                            // Only ever record these as PRODUCTION calls, and
                            // only from production context. Token matching is
                            // an over-approximation, so it must be able to
                            // suppress a finding but never to create one:
                            // crediting a spurious *test* call would push a
                            // function with no real callers into the report.
                            if !self.in_test() {
                                let caller = self.fn_stack.last().cloned().unwrap_or_default();
                                let n = id.to_string();
                                self.scan.edges.entry(caller).or_default().insert(n.clone());
                                self.scan.calls.entry(n).or_default().prod += 1;
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

    fn record_call(&mut self, name: String, span: Span) {
        let in_test = self.in_test();
        let file = self.file.to_path_buf();
        // Graph edge: whichever function we are currently inside calls `name`.
        let caller = self.fn_stack.last().cloned().unwrap_or_default();
        self.scan
            .edges
            .entry(caller)
            .or_default()
            .insert(name.clone());
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

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        // A macro body is an opaque token stream to syn, so calls written
        // inside one are invisible to the expression visitors. Codebases that
        // generate whole subsystems through macros (syscall tables, handler
        // registries) would otherwise look as though nothing calls them.
        //
        // Scan the tokens for `ident (` and count it as a call. This
        // over-approximates — a tuple-struct literal or a macro parameter can
        // match — but over-counting *calls* only ever suppresses a finding,
        // and a missed finding is far cheaper than a false accusation.
        self.record_tokens(node.tokens.clone());
        syn::visit::visit_macro(self, node);
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
    // Cargo knows exactly which files it compiles, including targets declared
    // in the manifest that live nowhere the convention would predict.
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

/// Walk a crate (or workspace), parse every `.rs` file, collect defs + calls.
pub fn scan_crate(root: &Path) -> anyhow::Result<Scan> {
    let roots = resolve_roots(root);
    let mut scan = Scan::default();
    for croot in &roots {
    for entry in walkdir::WalkDir::new(croot)
        .into_iter()
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
            fn_stack: Vec::new(),
            crate_root: croot.clone(),
        };
        v.visit_file(&ast);
    }
    }
    scan.crate_roots = roots;
    scan.config = crate::config::Config::load(root).unwrap_or_default();
    scan.workspace = crate::targets::discover(root);
    Ok(scan)
}

/// Names that are always reachable or conventionally unreferenced.
const ALWAYS_LIVE: &[&str] = &[
    "main", "new", "default", "fmt", "from", "from_str", "try_from", "drop", "clone",
    "next", "poll", "deref", "deref_mut", "eq", "ne", "hash", "cmp", "partial_cmp",
    "serialize", "deserialize", "into", "as_ref", "borrow",
];

/// How far the analyzer trusts a finding.
///
/// The distinction is not cosmetic. A function with no production call site
/// anywhere is dead by direct evidence — grep confirms it in seconds. A
/// function that *is* called from production code, whose callers all appear
/// unreachable, is either a genuine dead subsystem or a chain this analyzer
/// failed to resolve: async spawns, trait objects and stored closures all
/// break name-based edges, and everything downstream of the break is then
/// wrongly condemned.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub enum Confidence {
    /// No production caller exists anywhere.
    High,
    /// Production callers exist, but each of them also looks unreachable.
    Medium,
}

#[derive(Debug, serde::Serialize)]
pub struct Finding {
    pub name: String,
    pub file: String,
    pub line: usize,
    pub test_calls: usize,
    pub examples: Vec<String>,
    pub confidence: Confidence,
    /// Production call sites. Non-zero means Medium confidence.
    pub prod_calls: usize,
}

/// A production fn whose only callers are tests: it shipped, but nothing runs it.
pub fn never_run(scan: &Scan) -> Vec<Finding> {
    let mut out = Vec::new();
    for d in &scan.defs {
        if d.in_test || d.trait_impl || d.allowed_dead {
            continue;
        }
        if ALWAYS_LIVE.contains(&d.name.as_str()) || scan.config.is_ignored(&d.name) {
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
                    confidence: Confidence::High,
                    prod_calls: 0,
                });
            }
        }
    }
    out.sort_by(|a, b| b.test_calls.cmp(&a.test_calls));
    out
}

// ─── Call-graph reachability ──────────────────────────────────

/// Roots of the production-reachable set.
///
/// Deliberately generous: anything that could plausibly be entered from
/// outside the code we can see is a root, because treating a real entry point
/// as dead would produce a false accusation.
///
/// - `main` — the program's entry point
/// - `#[no_mangle]` / `extern` — callable from assembly or another language
/// - trait impl methods — reachable through dynamic dispatch
/// - names re-exported at the crate root — the library's public API
/// - `""` — calls made outside any function (static initialisers, consts)
pub fn production_roots(scan: &Scan) -> std::collections::HashSet<String> {
    let mut roots: std::collections::HashSet<String> = std::collections::HashSet::new();
    roots.insert(String::new());
    roots.insert("main".into());

    // Classify each crate: does it have a binary entry point of its own?
    let is_bin_crate = |croot: &Path| -> bool {
        croot.join("main.rs").is_file() || croot.join("bin").is_dir()
    };

    // Workspace-level question: is this thing an application or a library?
    //
    // An application has at least one binary. Its library crates are internal
    // plumbing — reached through that binary, not by outside consumers — so
    // their `pub` surface is NOT an entry point, and code nothing runs is
    // genuinely dead.
    //
    // A crate with no binary anywhere is a library. Its consumers are other
    // people's crates, which we cannot see, so its whole public API must be
    // treated as reachable or we would accuse the entire codebase. (Observed
    // before this rule: a 120-fn library reported 51% dead, all false.)
    let is_application = if scan.workspace.from_cargo {
        scan.workspace.is_application()
    } else {
        scan.crate_roots.iter().any(|r| is_bin_crate(r))
    };

    for d in &scan.defs {
        if d.in_test || d.is_test_fn {
            continue;
        }
        // A root the developer declared in landed.toml outranks every
        // heuristic: they know how their program is entered, and the analyzer
        // cannot see through a task spawn or a handler registry.
        let declared = scan.config.is_root(&d.name);
        let externally_reachable = if is_application {
            // Only a genuinely external surface counts: FFI symbols and
            // trait methods reached by dynamic dispatch.
            d.is_ffi || d.trait_impl
        } else {
            d.is_ffi || d.trait_impl || d.is_pub || scan.reexported.contains(&d.name)
        };
        if declared || externally_reachable {
            roots.insert(d.name.clone());
        }
    }
    roots
}

/// Was this scan of an application (has a binary) or a library?
pub fn is_application(scan: &Scan) -> bool {
    scan.crate_roots
        .iter()
        .any(|r| r.join("main.rs").is_file() || r.join("bin").is_dir())
}

/// Roots of the test-reachable set: every `#[test]`-style function, plus
/// every function defined inside a `#[cfg(test)]` module.
pub fn test_roots(scan: &Scan) -> std::collections::HashSet<String> {
    scan.defs
        .iter()
        .filter(|d| d.is_test_fn || d.in_test)
        .map(|d| d.name.clone())
        .collect()
}

/// Everything reachable from `roots` by following call edges transitively.
pub fn reachable(
    scan: &Scan,
    roots: &std::collections::HashSet<String>,
) -> std::collections::HashSet<String> {
    let mut seen: std::collections::HashSet<String> = roots.clone();
    let mut queue: Vec<String> = roots.iter().cloned().collect();
    while let Some(n) = queue.pop() {
        if let Some(callees) = scan.edges.get(&n) {
            for c in callees {
                if seen.insert(c.clone()) {
                    queue.push(c.clone());
                }
            }
        }
    }
    seen
}

/// Functions the tests can reach but the running program cannot.
///
/// This is the graph-wide generalisation of `never_run`: it catches an entire
/// dead subsystem, not merely its outermost function. A helper called only by
/// a dead function has a production call site and so looks alive to the
/// per-function check; here it is correctly unreachable.
pub fn never_run_graph(scan: &Scan) -> Vec<Finding> {
    let prod = reachable(scan, &production_roots(scan));
    let test = reachable(scan, &test_roots(scan));

    let mut out = Vec::new();
    for d in &scan.defs {
        if d.in_test || d.is_test_fn || d.trait_impl || d.allowed_dead || d.is_ffi {
            continue;
        }
        if ALWAYS_LIVE.contains(&d.name.as_str())
            || scan.reexported.contains(&d.name)
            || scan.config.is_ignored(&d.name)
        {
            continue;
        }
        // Name-based edges cannot distinguish two functions sharing a name,
        // so say nothing when the name is not unique in production code.
        if scan
            .defs
            .iter()
            .filter(|o| o.name == d.name && !o.in_test)
            .count()
            > 1
        {
            continue;
        }
        if !prod.contains(&d.name) && test.contains(&d.name) {
            let c = scan.calls.get(&d.name);
            let prod_calls = c.map(|c| c.prod).unwrap_or(0);
            out.push(Finding {
                name: d.name.clone(),
                file: d.file.display().to_string(),
                line: d.line,
                test_calls: c.map(|c| c.test).unwrap_or(0),
                examples: c.map(|c| c.examples.clone()).unwrap_or_default(),
                confidence: if prod_calls == 0 { Confidence::High } else { Confidence::Medium },
                prod_calls,
            });
        }
    }
    out.sort_by(|a, b| b.test_calls.cmp(&a.test_calls));
    out
}

/// Emit the call graph in Graphviz DOT, with unreachable nodes marked.
pub fn to_dot(scan: &Scan) -> String {
    let prod = reachable(scan, &production_roots(scan));
    let mut s = String::from("digraph calls {\n  rankdir=LR;\n  node [shape=box,fontsize=10];\n");
    for d in &scan.defs {
        if d.in_test || d.is_test_fn {
            continue;
        }
        let dead = !prod.contains(&d.name);
        s.push_str(&format!(
            "  \"{}\" [style=filled,fillcolor=\"{}\"];\n",
            d.name,
            if dead { "#ffd6d6" } else { "#e8f0e8" }
        ));
    }
    for (from, tos) in &scan.edges {
        if from.is_empty() {
            continue;
        }
        for to in tos {
            if scan.defs.iter().any(|d| &d.name == to && !d.in_test) {
                s.push_str(&format!("  \"{from}\" -> \"{to}\";\n"));
            }
        }
    }
    s.push_str("}\n");
    s
}

/// How much of the crate is invisible to name-based matching?
///
/// `never_run` stays silent whenever a name is defined more than once in
/// production, because a name-keyed graph cannot tell `A::process` from
/// `B::process`. That is the safe direction, but it is also a blind spot, and
/// its size should be known rather than assumed.
pub fn ambiguity_report(scan: &Scan) -> (usize, usize) {
    use std::collections::HashMap as M;
    let mut counts: M<&str, usize> = M::new();
    for d in &scan.defs {
        if !d.in_test {
            *counts.entry(d.name.as_str()).or_default() += 1;
        }
    }
    let total = counts.values().sum();
    let ambiguous: usize = counts.values().filter(|&&c| c > 1).sum();
    (ambiguous, total)
}

// ─── Dead regions ─────────────────────────────────────────────

/// A connected group of unreachable functions.
///
/// Reporting forty individual functions is noise; reporting three subsystems
/// with an entry point each is a work item. A region is a weakly-connected
/// component of the call graph induced on the unreachable set.
#[derive(Debug, serde::Serialize)]
pub struct Region {
    /// The frontier: where production reachability breaks. This is the
    /// function to investigate — everything else in the region is downstream
    /// of it and will become reachable, or disappear, once it is resolved.
    pub entry: Finding,
    /// Every other function in the region, nearest-first.
    pub members: Vec<Finding>,
    pub size: usize,
    /// Files the region spans, for a one-line locator.
    pub files: Vec<String>,
    /// The weakest confidence among the region's members.
    pub confidence: Confidence,
}

/// Group unreachable functions into connected regions and identify each
/// region's frontier.
pub fn dead_regions(scan: &Scan) -> Vec<Region> {
    use std::collections::{HashMap as M, HashSet as S};

    let findings = never_run_graph(scan);
    if findings.is_empty() {
        return Vec::new();
    }
    let by_name: M<&str, &Finding> = findings.iter().map(|f| (f.name.as_str(), f)).collect();
    let dead: S<&str> = by_name.keys().copied().collect();

    // Adjacency restricted to the dead set, plus its reverse.
    let mut fwd: M<&str, Vec<&str>> = M::new();
    let mut rev: M<&str, Vec<&str>> = M::new();
    for (from, tos) in &scan.edges {
        if !dead.contains(from.as_str()) {
            continue;
        }
        for to in tos {
            if let Some(t) = dead.get(to.as_str()) {
                if from != to {
                    fwd.entry(from.as_str()).or_default().push(t);
                    rev.entry(*t).or_default().push(from.as_str());
                }
            }
        }
    }

    // Weakly-connected components: walk both directions.
    let mut seen: S<&str> = S::new();
    let mut regions = Vec::new();
    let mut names: Vec<&str> = dead.iter().copied().collect();
    names.sort_unstable();

    for start in names {
        if seen.contains(start) {
            continue;
        }
        let mut component: Vec<&str> = Vec::new();
        let mut queue = vec![start];
        seen.insert(start);
        while let Some(n) = queue.pop() {
            component.push(n);
            for nb in fwd.get(n).into_iter().flatten().chain(rev.get(n).into_iter().flatten()) {
                if seen.insert(nb) {
                    queue.push(nb);
                }
            }
        }

        // The frontier is the member with no caller inside the region. If
        // several qualify (or none, in a cycle), prefer the one the tests
        // reach most directly — that is the way in.
        let entry_name = component
            .iter()
            .copied()
            .filter(|n| rev.get(n).map(|v| v.is_empty()).unwrap_or(true))
            .max_by_key(|n| by_name[n].test_calls)
            .or_else(|| component.iter().copied().max_by_key(|n| by_name[n].test_calls))
            .unwrap_or(start);

        let entry = clone_finding(by_name[entry_name]);
        let mut members: Vec<Finding> = component
            .iter()
            .filter(|n| **n != entry_name)
            .map(|n| clone_finding(by_name[n]))
            .collect();
        members.sort_by(|a, b| b.test_calls.cmp(&a.test_calls).then(a.name.cmp(&b.name)));

        let mut files: Vec<String> = std::iter::once(entry.file.clone())
            .chain(members.iter().map(|m| m.file.clone()))
            .collect();
        files.sort();
        files.dedup();

        // The frontier decides. A member's production callers are, by
        // construction, other members of the same region — that is what makes
        // it a region — so they say nothing about whether the region is
        // reachable. Only the way *in* matters: if the frontier is entered
        // solely from tests, everything behind it is unreachable too.
        let confidence = entry.confidence;
        regions.push(Region {
            size: 1 + members.len(),
            entry,
            members,
            files,
            confidence,
        });
    }

    regions.sort_by(|a, b| b.size.cmp(&a.size));
    regions
}

fn clone_finding(f: &Finding) -> Finding {
    Finding {
        name: f.name.clone(),
        file: f.file.clone(),
        line: f.line,
        test_calls: f.test_calls,
        examples: f.examples.clone(),
        confidence: f.confidence,
        prod_calls: f.prod_calls,
    }
}

// ─── Evidence ─────────────────────────────────────────────────

/// Why the analyzer reached its conclusion about one function.
///
/// A finding without evidence is an accusation. This is the record a
/// developer needs to confirm or refute it in under a minute.
pub struct Evidence {
    pub name: String,
    pub defined: Vec<(String, usize)>,
    pub in_production_set: bool,
    pub in_test_set: bool,
    pub is_root: bool,
    pub root_reason: &'static str,
    /// Callers, and whether each one is itself reachable from production.
    pub callers: Vec<(String, bool)>,
    pub prod_call_sites: usize,
    pub test_call_sites: usize,
    pub suppressed: Option<&'static str>,
}

pub fn evidence(scan: &Scan, name: &str) -> Evidence {
    let roots = production_roots(scan);
    let prod = reachable(scan, &roots);
    let test = reachable(scan, &test_roots(scan));

    let defs: Vec<&FnDef> = scan.defs.iter().filter(|d| d.name == name).collect();
    let d0 = defs.first();

    // Every function whose edge list contains this name.
    let mut callers: Vec<(String, bool)> = scan
        .edges
        .iter()
        .filter(|(_, tos)| tos.contains(name))
        .map(|(from, _)| {
            let label = if from.is_empty() { "<module level>".to_string() } else { from.clone() };
            (label, prod.contains(from))
        })
        .collect();
    callers.sort();
    callers.dedup();

    let root_reason = match d0 {
        Some(d) if d.is_ffi => "#[no_mangle] / extern",
        Some(d) if d.trait_impl => "trait impl method (dynamic dispatch)",
        Some(_) if scan.reexported.contains(name) => "re-exported at crate root",
        Some(d) if d.is_pub && !is_application(scan) => "public API of a library crate",
        _ if name == "main" => "program entry point",
        _ => "not a root",
    };

    let ambiguous = scan.defs.iter().filter(|o| o.name == name && !o.in_test).count() > 1;
    let suppressed = match d0 {
        None => Some("no definition found in the scanned crates"),
        Some(d) if d.in_test => Some("defined in test code"),
        Some(d) if d.allowed_dead => Some("#[allow(dead_code)]"),
        Some(d) if d.trait_impl => Some("trait impl method — dispatch is invisible here"),
        _ if ambiguous => Some("name is not unique in production; edges cannot be attributed"),
        _ if ALWAYS_LIVE.contains(&name) => Some("conventional entry point"),
        _ => None,
    };

    Evidence {
        name: name.to_string(),
        defined: defs.iter().map(|d| (d.file.display().to_string(), d.line)).collect(),
        in_production_set: prod.contains(name),
        in_test_set: test.contains(name),
        is_root: roots.contains(name),
        root_reason,
        callers,
        prod_call_sites: scan.calls.get(name).map(|c| c.prod).unwrap_or(0),
        test_call_sites: scan.calls.get(name).map(|c| c.test).unwrap_or(0),
        suppressed,
    }
}
