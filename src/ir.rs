//! Frontend-independent intermediate representation.
//!
//! The analysis layer must never learn which frontend produced an edge. If it
//! does, every consumer acquires a branch on frontend and the second frontend
//! becomes a rewrite rather than an addition.
//!
//! The types here degrade rather than branch. A syntactic frontend fills what
//! it can and declares the rest unknown; a compiler-resolved frontend fills
//! everything. Consumers ask *how far an edge can be trusted*, never *where it
//! came from*.
//!
//! See `docs/symbol-ir.md`.

use std::path::PathBuf;

/// How precisely a symbol or edge was resolved.
///
/// Ordered: `Nominal < Typed < Resolved`. A consumer that needs at least a
/// given precision can compare rather than match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub enum Precision {
    /// Name only. Two definitions sharing a name are indistinguishable, so
    /// nothing about either may be concluded.
    Nominal,
    /// Name plus the defining type. Distinguishes `A::process` from
    /// `B::process`.
    Typed,
    /// Resolved by the compiler. Exactly one definition, no ambiguity.
    Resolved,
}

/// Identity of a function, as precise as the frontend allows.
///
/// Only the fields a frontend can actually fill take part in identity, so a
/// definition and a call to it produce equal ids under the same frontend.
/// Source location deliberately lives on `Definition`, not here: two calls to
/// one function must not become two symbols.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
pub struct SymbolId {
    pub name: String,
    /// Defining type for a method — `A` in `A::process`. `None` when the
    /// frontend cannot see it.
    pub self_ty: Option<String>,
    /// Owning crate, for workspace-wide analysis.
    pub krate: Option<String>,
}

impl SymbolId {
    /// A name and nothing else — everything a syntactic frontend can promise.
    pub fn nominal(name: impl Into<String>) -> Self {
        Self { name: name.into(), self_ty: None, krate: None }
    }

    /// A method whose receiver type is known.
    pub fn typed(name: impl Into<String>, self_ty: impl Into<String>) -> Self {
        Self { name: name.into(), self_ty: Some(self_ty.into()), krate: None }
    }

    /// The precision this id carries on its own, before any frontend claim.
    pub fn precision(&self) -> Precision {
        match (&self.self_ty, &self.krate) {
            (Some(_), Some(_)) => Precision::Resolved,
            (Some(_), None) => Precision::Typed,
            _ => Precision::Nominal,
        }
    }
}

impl std::fmt::Display for SymbolId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.krate, &self.self_ty) {
            (Some(k), Some(t)) => write!(f, "{k}::{t}::{}", self.name),
            (None, Some(t)) => write!(f, "{t}::{}", self.name),
            _ => write!(f, "{}", self.name),
        }
    }
}

/// How one symbol reaches another.
///
/// The rules already differ by kind and are currently implicit in comments.
/// Naming them lets the difference be enforced by a match rather than
/// remembered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub enum EdgeKind {
    /// `foo()` — a path call.
    Call,
    /// `x.foo()` — receiver type may or may not be known.
    MethodCall,
    /// An identifier followed by `(` inside a macro body. An
    /// over-approximation: it may only ever suppress a finding, never create
    /// one, because token matching cannot tell a call from a tuple struct.
    MacroToken,
}

impl EdgeKind {
    /// May an edge of this kind be used as evidence that something is *live*?
    /// Always yes — that direction only removes findings.
    pub fn can_prove_live(&self) -> bool {
        true
    }

    /// May an edge of this kind be used as evidence that something is
    /// *reached by tests*, which can push a function into the report?
    ///
    /// Not for macro tokens: crediting a spurious test call would move a
    /// function with no real callers into the findings. Observed once —
    /// it moved one codebase from 11.76% to 14.04%.
    pub fn can_create_finding(&self) -> bool {
        !matches!(self, EdgeKind::MacroToken)
    }
}

/// A function definition, as one frontend saw it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Definition {
    pub id: SymbolId,
    /// How well this frontend resolved it.
    pub precision: Precision,
    pub file: PathBuf,
    pub line: usize,
    /// Defined inside `#[cfg(test)]`.
    pub in_test: bool,
    /// A `#[test]`-style harness function: a root of the test-reachable set,
    /// never of the production one.
    pub is_test_fn: bool,
    /// Method of a trait impl — reachable by dynamic dispatch, so the absence
    /// of a direct call site proves nothing.
    pub trait_impl: bool,
    /// Carries `#[allow(dead_code)]`; the author already decided.
    pub allowed_dead: bool,
    /// `pub` at the definition site. A library's public surface is an entry
    /// point; a binary's is not.
    pub is_pub: bool,
    /// `#[no_mangle]` / `extern` — callable from outside Rust entirely.
    pub is_ffi: bool,
    /// The crate `src/` this came from. Entry points are a property of a
    /// crate, not of a workspace.
    pub crate_root: PathBuf,
    /// Defining type, when the frontend saw one. Metadata rather than
    /// identity: the default frontend knows the type at a *definition* but
    /// not at a *call site*, so folding it into the id would stop the two
    /// matching. A precise frontend, which knows both, promotes it into `id`.
    pub self_ty: Option<String>,
}

impl Definition {
    /// Convenience: the bare name, whatever precision the id carries.
    pub fn name(&self) -> &str {
        &self.id.name
    }

    /// Graph key. Equals the bare name for a nominal id, so the default
    /// frontend behaves exactly as it did before ids existed.
    pub fn key(&self) -> String {
        self.id.to_string()
    }
}

/// One symbol invoking another.
#[derive(Debug, Clone)]
pub struct Edge {
    /// Caller. The empty-named symbol means module level — a static
    /// initialiser or const expression, outside any function.
    pub from: SymbolId,
    pub to: SymbolId,
    pub kind: EdgeKind,
    pub precision: Precision,
    /// Recorded in test context.
    pub in_test: bool,
    pub file: PathBuf,
    pub line: usize,
}

/// Everything a frontend extracted from a crate.
#[derive(Debug, Default)]
pub struct Extract {
    pub definitions: Vec<Definition>,
    pub edges: Vec<Edge>,
    /// Names re-exported at a crate root (`pub use`). Public API whose
    /// consumers are outside the tree, so absence of an in-crate caller
    /// proves nothing.
    pub reexported: std::collections::HashSet<String>,
    /// Crate `src/` directories that were read.
    pub crate_roots: Vec<PathBuf>,
}

/// A source of definitions and edges.
///
/// Implementations differ in precision and in what they require of the
/// environment; nothing downstream depends on which one ran.
pub trait Frontend {
    fn name(&self) -> &'static str;

    /// Best precision this frontend can produce, so a caller can choose
    /// without running it.
    fn precision(&self) -> Precision;

    fn extract(&self, root: &std::path::Path) -> anyhow::Result<Extract>;
}
