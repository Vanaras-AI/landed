# Resolved-symbol IR

The analysis layer must never learn which frontend produced an edge. If it
does, every consumer — reachability, regions, evidence, baselines, reporting —
acquires a branch on frontend, and the second frontend becomes a rewrite
rather than an addition.

Today `Scan` holds `HashMap<String, ...>` keyed on a bare function name. That
*is* the frontend leaking: a name is what syn can produce, and every downstream
decision is shaped by that limitation. `--stats` exists because of it.

## The unit

```rust
/// Identity of a function, as precise as the frontend that found it allows.
pub struct SymbolId {
    /// Bare name. Always present; the only thing syn can guarantee.
    pub name: String,
    /// Defining type for an inherent or trait method: `A` in `A::process`.
    pub self_ty: Option<String>,
    /// Trait, when the method comes from a trait impl.
    pub of_trait: Option<String>,
    /// Crate that owns the definition, for workspace-wide analysis.
    pub krate: Option<String>,
    /// Where it is defined. Distinguishes two same-named free functions in
    /// different modules when nothing else does.
    pub def_span: Option<Span>,
}
```

`SymbolId` degrades rather than branches. syn fills `name`, sometimes
`self_ty`, and always `def_span`. MIR fills all of it. Nothing downstream asks
which happened — it asks how precise the identity is:

```rust
pub enum Precision {
    /// Name only. Two definitions sharing it are indistinguishable.
    Nominal,
    /// Name plus defining type. Distinguishes A::process from B::process.
    Typed,
    /// Fully resolved by the compiler. One definition, no ambiguity.
    Resolved,
}
```

The existing `Confidence` is then derived from `Precision` rather than
hand-set, and the suppression rule stops being a special case:

| Precision of the edge | Effect on a finding |
|---|---|
| `Resolved` | judged normally, `Certain` |
| `Typed` | judged normally, `High`/`Medium` as today |
| `Nominal` and the name is unique | judged, `High`/`Medium` |
| `Nominal` and the name is not unique | **suppressed** — today's rule, now a consequence rather than an exception |

That last row is the 38–43%. It becomes a property of the data instead of an
`if` in `never_run`.

## The edge

```rust
pub struct Edge {
    pub from: SymbolId,
    pub to: SymbolId,
    pub precision: Precision,
    /// Direct call, method call, macro-body token match, dynamic dispatch.
    pub kind: EdgeKind,
    pub site: Span,
    /// Test or production context, decided by the frontend.
    pub in_test: bool,
}
```

`kind` matters because the rules already differ by it and are currently
implicit. A macro-token edge may only ever suppress a finding, never create
one — that rule lives in a comment in `record_tokens` today; here it is a
match arm on `EdgeKind::MacroToken`.

## What each frontend implements

```rust
pub trait Frontend {
    fn name(&self) -> &'static str;
    fn scan(&self, root: &Path) -> anyhow::Result<Vec<Definition>>;
    fn edges(&self, root: &Path) -> anyhow::Result<Vec<Edge>>;
    /// Best precision this frontend can produce, so a caller can choose
    /// without running it.
    fn precision(&self) -> Precision;
}
```

`SynFrontend` returns `Precision::Nominal` (occasionally `Typed`, when the
receiver is a plain path). `MirFrontend` returns `Resolved`. A future
`HirFrontend` would return `Resolved` too, and nothing else in the codebase
would change.

## Migration, in an order that never breaks the tool

1. Introduce `SymbolId` with only `name` populated, and key `Scan` on it
   instead of `String`. No behaviour changes; every test must stay green.
2. Move the ambiguity check to derive from `Precision` rather than counting
   duplicate names inline. `--stats` reports the same number by a different
   route.
3. Add `EdgeKind` and move the macro-token suppression rule from a comment
   into the type.
4. Extract `SynFrontend` behind the trait. Still no behaviour change.
5. Add `MirFrontend`, gated on `--precise`.

Steps 1–4 are refactors with an unchanged test suite as the proof. Only step 5
adds capability, and by then the frontend boundary already exists, so it is an
addition rather than surgery.

## The rule this exists to enforce

A consumer may ask **how much an edge can be trusted**. It may never ask
**where the edge came from**. The first is a property of the data; the second
is a dependency on a frontend, and it is how a second frontend turns into a
rewrite.
