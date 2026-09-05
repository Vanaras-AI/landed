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
            ],
            types: &["class_declaration", "class"],
            calls: &["call_expression", "new_expression"],
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

fn grammar(lang: Language) -> Option<tree_sitter::Language> {
    match lang {
        Language::Python => Some(tree_sitter_python::LANGUAGE.into()),
        Language::TypeScript => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
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
        let grammar = grammar(lang)
            .ok_or_else(|| anyhow::anyhow!("no tree-sitter grammar for {}", lang.name()))?;
        let spec = spec(lang);

        let mut parser = Parser::new();
        parser
            .set_language(&grammar)
            .map_err(|e| anyhow::anyhow!("could not load the {} grammar: {e}", lang.name()))?;

        let mut out = Extract::default();
        for path in project.source_files() {
            let src = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let tree = match parser.parse(&src, None) {
                Some(t) => t,
                // Unparseable file: skip it, do not fail the run. The same
                // rule the Rust frontend follows.
                None => continue,
            };
            let mut w = Walker {
                out: &mut out,
                src: src.as_bytes(),
                spec: &spec,
                file: &path,
                module: module_path(&path, root, lang),
                in_test: project.is_test_file(&path),
                fn_stack: Vec::new(),
                type_stack: Vec::new(),
            };
            w.walk(tree.root_node());
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

struct Walker<'a> {
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

    fn named(&self, n: Node) -> Option<String> {
        self.text(n.child_by_field_name("name")?)
    }

    /// The identifier a call ultimately names.
    ///
    /// `foo()` is an identifier; `obj.foo()` is an attribute or member access
    /// whose last segment is the name. Anything else — calling the result of
    /// an expression, an index, a computed member — is not a name, and is not
    /// guessed at.
    fn callee(&self, n: Node) -> Option<String> {
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
                self.out.definitions.push(Definition {
                    id: SymbolId::nominal(name.clone()),
                    precision: Precision::Nominal,
                    file: self.file.to_path_buf(),
                    line: node.start_position().row + 1,
                    in_test: self.in_test,
                    // A test *function* by convention, so a whole file need
                    // not be test code for its tests to count as tests.
                    is_test_fn: self.in_test || name.starts_with("test_") || name == "setUp",
                    // No dynamic dispatch is modelled here, so nothing is
                    // exempted on that basis.
                    trait_impl: false,
                    allowed_dead: false,
                    // Python and Go use a leading underscore for private; TS
                    // has no marker in the syntax tree worth trusting.
                    is_pub: !name.starts_with('_'),
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

        if self.spec.calls.contains(&kind) {
            if let Some(to) = self.callee(node) {
                let from = SymbolId::nominal(match self.fn_stack.last() {
                    Some(f) => f.clone(),
                    // Module level. In a test file that is not a production
                    // entry point, however much it looks like one.
                    None if self.in_test => TEST_MODULE_ROOT.to_string(),
                    None => String::new(),
                });
                self.out.edges.push(Edge {
                    from,
                    to: SymbolId::nominal(to),
                    kind: EdgeKind::Call,
                    precision: Precision::Nominal,
                    in_test: self.in_test,
                    file: self.file.to_path_buf(),
                    line: node.start_position().row + 1,
                });
            }
        }

        self.walk_children(node);
    }

    fn walk_children(&mut self, node: Node) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.walk(child);
        }
    }
}
