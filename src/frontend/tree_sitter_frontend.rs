//! Syntactic frontend for every language that is not Rust.
//!
//! Tree-sitter gives a concrete syntax tree per language from one dependency.
//! What the analyzer needs from a tree is small and nearly identical
//! everywhere — where functions are defined, what they call, which class a
//! method belongs to — so one walker is parameterised by a per-language
//! description of the node kinds rather than written four times.
//!
//! Precision is `Nominal`, the same as the Rust default tier: tree-sitter
//! parses, it does not resolve. A method keeps its enclosing class as
//! metadata, which the analysis may use to tell two same-named methods apart
//! once a call site can name the receiver — it cannot here, so the ambiguity
//! is reported rather than guessed at, exactly as in Rust.

use crate::ir::*;
use crate::lang::Language;
use std::collections::HashSet;
use std::path::Path;
use tree_sitter::{Node, Parser};

pub struct TreeSitterFrontend {
    pub language: Language,
}

/// Node kinds that carry meaning, per language.
struct Spec {
    /// Kinds that declare a callable.
    functions: &'static [&'static str],
    /// Kinds that introduce a type whose methods belong to it.
    types: &'static [&'static str],
    /// Kinds that invoke something.
    calls: &'static [&'static str],
    /// Field holding the callee expression on a call node.
    callee_field: &'static str,
}

fn spec(lang: Language) -> Spec {
    match lang {
        Language::Python => Spec {
            functions: &["function_definition"],
            types: &["class_definition"],
            calls: &["call"],
            callee_field: "function",
        },
        Language::TypeScript => Spec {
            functions: &[
                "function_declaration",
                "function_expression",
                "generator_function_declaration",
                "method_definition",
                // `const f = () => {}` is the dominant idiom in modern
                // TypeScript, and carries no name of its own — the name is on
                // the binding above it. Without these two kinds the walker
                // reads a handful of functions in a codebase of hundreds and
                // reports the rest of it clean.
                "arrow_function",
                "generator_function",
            ],
            types: &["class_declaration", "class"],
            // `<Chart />` is how a React component is used. It is not a
            // call_expression, and without these kinds every component in a
            // codebase looks as though nothing refers to it.
            calls: &[
                "call_expression",
                "new_expression",
                "jsx_self_closing_element",
                "jsx_opening_element",
            ],
            callee_field: "function",
        },
        Language::Go => Spec {
            functions: &["function_declaration", "method_declaration"],
            types: &["type_declaration"],
            calls: &["call_expression"],
            callee_field: "function",
        },
        // Rust has its own frontend; this exists so the match is total.
        Language::Rust => Spec {
            functions: &["function_item"],
            types: &["impl_item"],
            calls: &["call_expression"],
            callee_field: "function",
        },
    }
}

/// The grammar for a language, and for TypeScript the dialect matching the
/// file. TSX is a separate grammar, not a superset flag: parsing a `.tsx` file
/// with the plain TypeScript grammar yields errors where the JSX begins, and
/// every component below that point is silently unread.
fn grammar(lang: Language, file: &Path) -> Option<tree_sitter::Language> {
    match lang {
        Language::Python => Some(tree_sitter_python::LANGUAGE.into()),
        Language::TypeScript => {
            // Only `.ts`, `.mts` and `.cts` get the TypeScript grammar. It is
            // the one dialect that must: `<T>expr` is a type assertion there
            // and an unclosed JSX tag under the other grammar.
            //
            // Everything else gets TSX, which parses plain JavaScript as well
            // as JSX. Guessing by extension is not enough for `.js` — React
            // projects put JSX in `.js` constantly — and the TSX grammar
            // reads both, so it is the safe default rather than a compromise.
            let strict_ts = matches!(
                file.extension().and_then(|e| e.to_str()),
                Some("ts") | Some("mts") | Some("cts")
            );
            Some(if strict_ts {
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
            } else {
                tree_sitter_typescript::LANGUAGE_TSX.into()
            })
        }
        Language::Go => Some(tree_sitter_go::LANGUAGE.into()),
        Language::Rust => None,
    }
}

impl Frontend for TreeSitterFrontend {
    fn name(&self) -> &'static str {
        "tree-sitter"
    }

    fn precision(&self) -> Precision {
        Precision::Nominal
    }

    fn extract(&self, root: &Path) -> anyhow::Result<Extract> {
        let lang = self.language;
        let project = crate::project::detect_as(root, Some(lang));
        let spec = spec(lang);

        let mut parser = Parser::new();
        let mut loaded: Option<tree_sitter::Language> = None;

        let files = project.source_files();

        // Python states its public surface explicitly or not at all.
        let exports = if lang == Language::Python {
            let mut pre = Parser::new();
            pre.set_language(&tree_sitter_python::LANGUAGE.into())
                .map_err(|e| anyhow::anyhow!("could not load the python grammar: {e}"))?;
            python_exports(&files, &mut pre)
        } else {
            HashSet::new()
        };

        let mut strings: Vec<String> = Vec::new();
        let mut out = Extract {
            reexported: exports.clone(),
            ..Default::default()
        };
        for path in &files {
            let src = match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let want = grammar(lang, path)
                .ok_or_else(|| anyhow::anyhow!("no tree-sitter grammar for {}", lang.name()))?;
            if loaded.as_ref() != Some(&want) {
                parser.set_language(&want).map_err(|e| {
                    anyhow::anyhow!("could not load the {} grammar: {e}", lang.name())
                })?;
                loaded = Some(want);
            }
            let tree = match parser.parse(&src, None) {
                Some(t) => t,
                // Unparseable file: skip it, do not fail the run. The same
                // rule the Rust frontend follows.
                None => continue,
            };
            let mut w = Walker {
                lang,
                strings: &mut strings,
                out: &mut out,
                src: src.as_bytes(),
                spec: &spec,
                file: path,
                module: module_path(path, root, lang),
                in_test: project.is_test_file(path),
                exports: &exports,
                fn_stack: Vec::new(),
                type_stack: Vec::new(),
            };
            w.walk(tree.root_node());
        }
        // A name that only ever appears inside a template or a lookup key is
        // called by reflection at run time, and nothing in the graph records
        // who calls it. `{{if .HasHelpSubCommands}}` is a real example: the
        // method is dispatched from a template string, so a change to it is
        // reachable from tests that no edge connects to it.
        //
        // Marked rather than guessed at. The caller widens its answer instead
        // of narrowing it, which is why only two shapes count: a string that
        // is a template, and a string that is exactly the name — a lookup
        // key. A name merely mentioned in a log line is not enough, or every
        // function would be marked and the answer would always be "run
        // everything".
        let templates: Vec<&String> = strings.iter().filter(|s| s.contains("{{")).collect();
        for d in &mut out.definitions {
            if d.in_test {
                continue;
            }
            let n = d.id.name.as_str();
            let in_template = templates.iter().any(|t| t.contains(n));
            let is_a_key = strings
                .iter()
                .any(|t| t.trim_matches(['"', '`', '\'']) == n);
            if in_template || is_a_key {
                d.opaque = true;
            }
        }

        out.crate_roots = vec![root.to_path_buf()];
        Ok(out)
    }
}

/// Dotted module path from the file's position under the root, so two
/// same-named functions in different modules stay distinguishable as metadata.
fn module_path(file: &Path, root: &Path, lang: Language) -> Option<String> {
    let rel = file.strip_prefix(root).ok()?;
    let mut parts: Vec<String> = rel
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(str::to_string))
        .collect();
    let last = parts.pop()?;
    let stem = last.rsplit_once('.').map(|(s, _)| s).unwrap_or(&last);
    // A package or barrel file names the directory, not itself.
    let is_index = matches!(stem, "__init__" | "index" | "mod" | "lib" | "main");
    if !is_index {
        parts.push(stem.to_string());
    }
    let _ = lang;
    (!parts.is_empty()).then(|| parts.join("::"))
}

/// The names a Python project declares as its public surface.
///
/// Python has no `pub`. The convention this frontend first used — "no leading
/// underscore" — makes almost every function public, so every function of a
/// library becomes a root and the analysis can never say anything. That is
/// vacuous rather than safe.
///
/// What Python does have is an explicit export list. A name in `__all__`, or
/// imported into a package's `__init__.py`, is the API the author meant to
/// publish; everything else is internal however it is spelled.
///
/// Returns an empty set when the project declares nothing, and the caller
/// then falls back to the underscore convention — a project with no stated
/// API has not told us anything, and guessing narrowly there would invent
/// findings.
fn python_exports(files: &[std::path::PathBuf], parser: &mut Parser) -> HashSet<String> {
    let mut out = HashSet::new();
    for path in files {
        let is_init = path
            .file_name()
            .map(|f| f == "__init__.py")
            .unwrap_or(false);
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        let Some(tree) = parser.parse(&src, None) else {
            continue;
        };
        let bytes = src.as_bytes();
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            match n.kind() {
                // `__all__ = ["a", "b"]`, anywhere.
                "assignment" => {
                    let named = n
                        .child_by_field_name("left")
                        .and_then(|l| l.utf8_text(bytes).ok())
                        .map(|t| t.trim() == "__all__")
                        .unwrap_or(false);
                    if named {
                        if let Some(r) = n.child_by_field_name("right") {
                            let mut inner = vec![r];
                            while let Some(m) = inner.pop() {
                                if m.kind() == "string" {
                                    if let Ok(t) = m.utf8_text(bytes) {
                                        let name = t.trim_matches(['"', '\'']);
                                        if !name.is_empty() {
                                            out.insert(name.to_string());
                                        }
                                    }
                                }
                                let mut c = m.walk();
                                inner.extend(m.children(&mut c));
                            }
                        }
                    }
                }
                // A package root re-exports what it imports.
                "import_from_statement" | "import_statement" if is_init => {
                    let mut c = n.walk();
                    for child in n.children(&mut c) {
                        match child.kind() {
                            "dotted_name" | "identifier" => {
                                if let Ok(t) = child.utf8_text(bytes) {
                                    if let Some(last) = t.rsplit('.').next() {
                                        out.insert(last.to_string());
                                    }
                                }
                            }
                            "aliased_import" => {
                                if let Some(a) = child.child_by_field_name("alias") {
                                    if let Ok(t) = a.utf8_text(bytes) {
                                        out.insert(t.to_string());
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            let mut c = n.walk();
            stack.extend(n.children(&mut c));
        }
    }
    out
}

/// Does this body reach code through something no call edge crosses?
///
/// A test that runs a subprocess, opens a socket or drives a browser
/// exercises the program from outside, and the call graph shows it calling
/// almost nothing. Reading that as "affected by nothing" is how a selection
/// tool skips exactly the tests that catch integration bugs.
///
/// Matched against the function's source text rather than its parse tree, so
/// a mention in a comment or a string counts too. That over-triggers, and
/// over-triggering is the safe direction: a test wrongly marked here is
/// merely always run.
fn crosses_a_boundary(text: &str, lang: Language) -> bool {
    let markers: &[&str] = match lang {
        Language::Python => &[
            "subprocess",
            "Popen",
            "os.system",
            "os.exec",
            "pexpect",
            "requests.",
            "httpx.",
            "aiohttp",
            "urllib.request",
            "socket.",
            "selenium",
            "webdriver",
            "docker",
            "psycopg",
            "pymongo",
        ],
        Language::Go => &[
            "exec.Command",
            "os/exec",
            "httptest.",
            "http.Get",
            "http.Post",
            "net.Dial",
            "net.Listen",
            "sql.Open",
            "testcontainers",
        ],
        Language::TypeScript => &[
            "child_process",
            "execSync",
            "spawnSync",
            "spawn(",
            "execa",
            "puppeteer",
            "playwright",
            "supertest",
            "axios.",
            "fetch(",
            "request(",
            "WebSocket",
            "testcontainers",
        ],
        Language::Rust => &[],
    };
    markers.iter().any(|m| text.contains(m))
}

struct Walker<'a> {
    lang: Language,
    /// Text of every string literal seen, for spotting names that are only
    /// ever reached by reflection.
    strings: &'a mut Vec<String>,
    /// Names the project states are its public API. Empty means it stated
    /// none, and the underscore convention stands in.
    exports: &'a HashSet<String>,
    out: &'a mut Extract,
    src: &'a [u8],
    spec: &'a Spec,
    file: &'a Path,
    module: Option<String>,
    in_test: bool,
    fn_stack: Vec<String>,
    type_stack: Vec<String>,
}

impl<'a> Walker<'a> {
    fn text(&self, n: Node) -> Option<String> {
        n.utf8_text(self.src).ok().map(str::to_string)
    }

    /// The name a callable is known by.
    ///
    /// Most declarations carry their own. A function *value* does not: in
    /// `const handle = () => {}` the name belongs to the binding above, and
    /// the same holds for a class field, an object-literal property, a Python
    /// assignment and a Go `var`. Callers write that name, so that is the
    /// name the graph must key on.
    ///
    /// A function value with no binding above it — a callback argument, an
    /// IIFE — has no name and gets no definition. Its body still belongs to
    /// the function that wrote it, which is what happens when this returns
    /// `None`.
    fn named(&self, n: Node) -> Option<String> {
        if let Some(own) = n.child_by_field_name("name") {
            return self.text(own);
        }
        let parent = n.parent()?;
        let field = match parent.kind() {
            "variable_declarator" | "public_field_definition" | "field_definition" => "name",
            "pair" => "key",
            // Python `f = lambda: ...`, Go `f := func() {}`.
            "assignment" | "short_var_declaration" => "left",
            "var_spec" | "const_spec" => "name",
            _ => return None,
        };
        let name = parent.child_by_field_name(field)?;
        // Only a plain identifier is a name. Destructuring, a computed key
        // and a member assignment are not, and are not guessed at.
        matches!(
            name.kind(),
            "identifier" | "property_identifier" | "type_identifier"
        )
        .then(|| self.text(name))
        .flatten()
    }

    /// The identifier a call ultimately names.
    ///
    /// `foo()` is an identifier; `obj.foo()` is an attribute or member access
    /// whose last segment is the name. Anything else — calling the result of
    /// an expression, an index, a computed member — is not a name, and is not
    /// guessed at.
    fn callee(&self, n: Node) -> Option<String> {
        if n.kind().starts_with("jsx_") {
            let name = n.child_by_field_name("name")?;
            let text = self.text(name)?;
            // `<div>` is an HTML element; `<Chart>` is a component defined
            // somewhere in this project. The capital is how JSX itself tells
            // them apart, and it is the only signal there is.
            let is_component = text
                .chars()
                .next()
                .map(|c| c.is_uppercase())
                .unwrap_or(false);
            // `<Ns.Thing />` names Thing.
            return is_component.then(|| text.rsplit('.').next().unwrap_or(&text).to_string());
        }
        let f = n.child_by_field_name(self.spec.callee_field)?;
        match f.kind() {
            "identifier" | "type_identifier" | "field_identifier" => self.text(f),
            "attribute" | "member_expression" | "selector_expression" => {
                let last = f
                    .child_by_field_name("attribute")
                    .or_else(|| f.child_by_field_name("property"))
                    .or_else(|| f.child_by_field_name("field"))?;
                self.text(last)
            }
            _ => None,
        }
    }

    /// Is this definition part of what the outside world can reach?
    ///
    /// It decides the root set for a library, so a rule that is too generous
    /// makes every function a root and the analysis vacuous — which is what
    /// one borrowed convention did across all four languages. Each language
    /// states this its own way, and each states it exactly.
    fn is_exported(&self, node: Node, name: &str) -> bool {
        match self.lang {
            // The compiler enforces this one: a capitalised identifier is
            // exported from its package, and nothing else is.
            Language::Go => name.chars().next().map(char::is_uppercase).unwrap_or(false),
            // `export` is the marker, and it sits above the declaration —
            // above the binding too, for `export const f = () => {}`.
            Language::TypeScript => {
                let mut n = Some(node);
                while let Some(cur) = n {
                    if matches!(cur.kind(), "export_statement" | "export_clause") {
                        return true;
                    }
                    n = cur.parent();
                }
                false
            }
            // An explicit export list, but only where it can speak.
            //
            // `__all__` and a package's `__init__.py` name modules, classes
            // and free functions. They never name a *method*: `app.run` and
            // `@app.before_request` are as public as an API gets, and neither
            // appears in any export list anywhere. Judging methods by that
            // list marked the whole decorator API of a web framework private,
            // and reported it dead.
            //
            // So the list governs what it describes — top-level functions —
            // and a method falls back to the only signal Python offers for
            // one, which is the leading underscore.
            Language::Python => {
                if self.type_stack.is_empty() && !self.exports.is_empty() {
                    self.exports.contains(name)
                } else {
                    !name.starts_with('_')
                }
            }
            Language::Rust => !name.starts_with('_'),
        }
    }

    fn walk(&mut self, node: Node) {
        let kind = node.kind();

        if self.spec.types.contains(&kind) {
            if let Some(name) = self.named(node) {
                self.type_stack.push(name);
                self.walk_children(node);
                self.type_stack.pop();
                return;
            }
        }

        if self.spec.functions.contains(&kind) {
            if let Some(name) = self.named(node) {
                let self_ty = self.type_stack.last().cloned();
                let lang = self.lang;
                self.out.definitions.push(Definition {
                    id: SymbolId::nominal(name.clone()),
                    precision: Precision::Nominal,
                    file: self.file.to_path_buf(),
                    line: node.start_position().row + 1,
                    in_test: self.in_test,
                    // A test *function* by convention, so a whole file need
                    // not be test code for its tests to count as tests.
                    is_test_fn: self.in_test || name.starts_with("test_") || name == "setUp",
                    opaque: self
                        .text(node)
                        .map(|t| crosses_a_boundary(&t, lang))
                        .unwrap_or(false),
                    // A Python dunder is called by the language, not by
                    // name: `__call__` runs when an instance is applied,
                    // `__iter__` when it is looped over, and neither ever
                    // appears at a call site. That is dynamic dispatch by
                    // another spelling, and it is exempted on the same
                    // grounds a trait method is — a missing call site proves
                    // nothing about it.
                    //
                    // Without this, a web framework's WSGI entry point is
                    // reported dead, which is true of the text and false of
                    // the program.
                    trait_impl: (lang == Language::Python
                        && name.starts_with("__")
                        && name.ends_with("__"))
                        // A function written as an object-literal property —
                        // `{ clearStorage: () => {...} }` — is reached by
                        // property access on a value this tier cannot follow.
                        // Nothing in the repository calls it by name, and its
                        // absence from the call graph proves nothing.
                        || node
                            .parent()
                            .map(|p| p.kind() == "pair")
                            .unwrap_or(false)
                        // Go interfaces are structural: a type satisfies one
                        // by having the methods, with nothing written down to
                        // say so. Any exported method may therefore be
                        // reached through an interface this tier cannot see,
                        // and the same is true of one registered by name into
                        // a template or handler map and invoked by reflection.
                        //
                        // Rust states its dispatch — `impl Trait for T` — and
                        // only those methods are exempted. Go states nothing,
                        // so the exemption has to cover every exported
                        // method. That costs real findings, and the
                        // alternative is accusing live code with no way for
                        // the reader to tell which is which.
                        || (lang == Language::Go
                            && kind == "method_declaration"
                            && name.chars().next().map(char::is_uppercase).unwrap_or(false)),
                    allowed_dead: false,
                    is_pub: self.is_exported(node, &name),
                    is_ffi: false,
                    crate_root: self.file.to_path_buf(),
                    self_ty,
                    module: self.module.clone(),
                });
                self.fn_stack.push(name);
                self.walk_children(node);
                self.fn_stack.pop();
                return;
            }
        }

        // `const api = { setState, subscribe }` hands two functions to a
        // caller this tier cannot follow. The shorthand is not a call, but it
        // is unmistakably a use, and treating it as an edge from the
        // enclosing function over-approximates liveness — which is the
        // direction this analysis is required to fail in.
        //
        // Without it, a store library's whole public surface, handed out as
        // object properties, reads as dead.
        if matches!(
            kind,
            "string" | "raw_string_literal" | "interpreted_string_literal"
        ) {
            if let Some(t) = self.text(node) {
                self.strings.push(t);
            }
        }

        if kind == "shorthand_property_identifier" {
            if let Some(name) = self.text(node) {
                self.record_edge(&name, node);
            }
        }

        // `ValidArgsFunction: NoFileCompletions` — a function handed over as a
        // value, to be called by whoever received it. Not a call site, and
        // unmistakably a use. Missing it made a completion helper look
        // unaffected by any change, so the test that covers it was not
        // selected.
        //
        // Restricted to positions where a value is genuinely being passed or
        // stored. Matching every identifier anywhere would let a local
        // variable that happens to share a function's name suppress a real
        // finding.
        if kind == "identifier" && self.names_a_value(node) {
            if let Some(name) = self.text(node) {
                self.record_reference(&name, node);
            }
        }

        // `test("name", () => {...})` — a test in JavaScript and TypeScript is
        // an anonymous callback passed to a runner, not a named function.
        // Counting only named functions in test files counts the helpers and
        // misses every actual test, which makes any answer about "which tests
        // are affected" meaningless in the language's dominant idiom.
        if self.lang == Language::TypeScript && kind == "call_expression" {
            if let Some(name) = self.declared_test(node) {
                self.out.definitions.push(Definition {
                    id: SymbolId::nominal(name.clone()),
                    precision: Precision::Nominal,
                    file: self.file.to_path_buf(),
                    line: node.start_position().row + 1,
                    in_test: true,
                    is_test_fn: true,
                    trait_impl: false,
                    allowed_dead: false,
                    is_pub: false,
                    is_ffi: false,
                    opaque: self
                        .text(node)
                        .map(|t| crosses_a_boundary(&t, self.lang))
                        .unwrap_or(false),
                    crate_root: self.file.to_path_buf(),
                    self_ty: None,
                    module: self.module.clone(),
                });
                self.fn_stack.push(name);
                self.walk_children(node);
                self.fn_stack.pop();
                return;
            }
        }

        if self.spec.calls.contains(&kind) {
            if let Some(to) = self.callee(node) {
                self.record_edge(&to, node);
            }
        }

        self.walk_children(node);
    }

    /// A call that declares a test: `test("name", fn)`, `it("name", fn)`.
    ///
    /// Returns the name the runner will print, which is what a selection
    /// answer has to name for anyone to act on it.
    fn declared_test(&self, n: Node) -> Option<String> {
        let callee = n.child_by_field_name("function")?;
        let which = self.text(callee)?;
        let base = which.rsplit('.').next().unwrap_or(&which);
        if !matches!(base, "test" | "it") {
            return None;
        }
        let args = n.child_by_field_name("arguments")?;
        let mut cur = args.walk();
        let mut children = args.named_children(&mut cur);
        let first = children.next()?;
        if first.kind() != "string" {
            return None;
        }
        // A test declared with no body is a placeholder, not a test.
        let has_body = children.any(|c| {
            matches!(
                c.kind(),
                "arrow_function" | "function_expression" | "function"
            )
        });
        if !has_body {
            return None;
        }
        let raw = self.text(first)?;
        let name = raw.trim_matches(['"', '\'', '`']).trim().to_string();
        (!name.is_empty()).then_some(name)
    }

    /// Is this identifier being passed or stored, rather than called?
    fn names_a_value(&self, n: Node) -> bool {
        let Some(parent) = n.parent() else {
            return false;
        };
        // The callee of a call is already an edge; recording it twice is
        // harmless but pointless, and the field check keeps this honest.
        if matches!(parent.kind(), "call_expression" | "call")
            && parent.child_by_field_name("function").map(|f| f.id()) == Some(n.id())
        {
            return false;
        }
        // The name being *bound*, not a value being passed. `const unused =
        // () => {}` has its own name in a declarator, and reading that as a
        // use makes every definition reference itself — at module level that
        // is a production root, so nothing is ever unreachable again.
        for field in ["name", "left", "key"] {
            if parent.child_by_field_name(field).map(|f| f.id()) == Some(n.id()) {
                return false;
            }
        }
        matches!(
            parent.kind(),
            // struct and object literals
            "keyed_element"
                | "literal_value"
                | "pair"
                // passed along
                | "argument_list"
                | "arguments"
                // stored
                | "assignment"
                | "assignment_statement"
                | "short_var_declaration"
                | "var_spec"
                | "const_spec"
                | "variable_declarator"
                // handed back
                | "return_statement"
                | "expression_list"
                | "array"
                | "list"
        )
    }

    /// A use that is not a call.
    ///
    /// Tagged so it behaves like a macro token: it may suppress a finding,
    /// never create one, and it is always visible to test selection.
    fn record_reference(&mut self, to: &str, at: Node) {
        let from = SymbolId::nominal(match self.fn_stack.last() {
            Some(f) => f.clone(),
            None if self.in_test => TEST_MODULE_ROOT.to_string(),
            None => String::new(),
        });
        self.out.edges.push(Edge {
            from,
            to: SymbolId::nominal(to.to_string()),
            kind: EdgeKind::MacroToken,
            precision: Precision::Nominal,
            in_test: self.in_test,
            file: self.file.to_path_buf(),
            line: at.start_position().row + 1,
        });
    }

    fn record_edge(&mut self, to: &str, at: Node) {
        let from = SymbolId::nominal(match self.fn_stack.last() {
            Some(f) => f.clone(),
            // Module level. In a test file that is not a production entry
            // point, however much it looks like one.
            None if self.in_test => TEST_MODULE_ROOT.to_string(),
            None => String::new(),
        });
        self.out.edges.push(Edge {
            from,
            to: SymbolId::nominal(to.to_string()),
            kind: EdgeKind::Call,
            precision: Precision::Nominal,
            in_test: self.in_test,
            file: self.file.to_path_buf(),
            line: at.start_position().row + 1,
        });
    }

    fn walk_children(&mut self, node: Node) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.walk(child);
        }
    }
}
