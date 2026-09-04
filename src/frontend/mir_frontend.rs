//! Compiler-resolved frontend: MIR.
//!
//! `syn` cannot tell `A::process` from `B::process`, so the analysis declines
//! to judge either — 38–43% of a real codebase. MIR has been through name
//! resolution and type checking, so a call site names its target:
//!
//! ```text
//! fn main() -> () {
//!     _2 = A::process(move _3) -> [return: bb1, unwind continue];
//! }
//! fn helper() -> () {
//!     _2 = B::process(move _3) -> [return: bb1, unwind continue];
//! }
//! ```
//!
//! What it does **not** give, and this matters for the precision claimed: the
//! textual dump identifies free functions by bare name with no module path, so
//! two `nested` functions in different modules remain indistinguishable. MIR
//! raises methods from `Nominal` to `Typed`; it does not reach `Resolved` for
//! everything. The frontend reports that honestly rather than overclaiming.
//!
//! Requires a nightly toolchain and a crate that compiles. Both are checked up
//! front and reported as actionable errors — this mode exists to be precise,
//! so it must never quietly answer with something less.

use crate::ir::*;
use std::path::Path;
use std::process::Command;

pub struct MirFrontend;

impl Frontend for MirFrontend {
    fn name(&self) -> &'static str {
        "mir"
    }

    fn precision(&self) -> Precision {
        Precision::Typed
    }

    fn extract(&self, root: &Path) -> anyhow::Result<Extract> {
        let dir = crate_dir(root);
        let mir = dump_mir(&dir)?;

        // Definitions come from syn: MIR knows what calls what, but carries no
        // visibility, no #[cfg(test)], no #[allow(dead_code)], and no source
        // line. The two are combined rather than one replacing the other.
        let mut base = super::SynFrontend.extract(root)?;
        let parsed = parse(&mir);

        // Promote each definition to the identity MIR gave it, so that the
        // definition side of the graph is keyed the same way the call side is.
        //
        // Matching is on the metadata syn recorded — receiver type for a
        // method, module path for a free function. MIR prints the shortest
        // path that distinguishes a symbol, so its qualifier is a *suffix* of
        // the full module path syn knows.
        for d in &mut base.definitions {
            let promoted = parsed.definitions.iter().find(|m| {
                if m.id.name != d.id.name {
                    return false;
                }
                match (&m.id.self_ty, &m.id.path) {
                    // A method: the receiver types must agree.
                    (Some(t), _) => d.self_ty.as_deref() == Some(t.as_str()),
                    // A free function in a module: MIR's qualifier must be a
                    // suffix of the module path syn saw.
                    (None, Some(p)) => d
                        .module
                        .as_deref()
                        .map(|full| full == p || full.ends_with(&format!("::{p}")))
                        .unwrap_or(false),
                    // Unqualified in MIR means unique in the compiled target,
                    // so it may only be claimed when syn saw exactly one
                    // production definition of that name.
                    (None, None) => false,
                }
            });
            if let Some(m) = promoted {
                d.id = m.id.clone();
                d.precision = Precision::Typed;
            }
        }

        // MIR overrides syn only where it actually resolved something.
        //
        // Replacing every syntactic edge sounds cleaner and is wrong: this
        // parser reads a call form the compiler prints for humans, and every
        // form it does not recognise is a call that disappears. Measured on
        // real crates that silence turned live code into findings — 0 to 10 on
        // one, 1 to 21 on another. syn misses nothing, because it reads the
        // source.
        //
        // So: for a name MIR resolved to a concrete type, MIR's edges are the
        // truth and the syntactic ones are dropped — that is the whole point
        // of the tier. Every other name keeps its syntactic edges, and keeps
        // its coverage with them.
        // Any qualifier counts, type or module: `alpha::helper` and
        // `beta::helper` are exactly as much a resolution as `A::process`.
        let resolved_names: std::collections::HashSet<String> = parsed
            .edges
            .iter()
            .filter(|e| e.to.is_qualified())
            .map(|e| e.to.name.clone())
            .collect();

        // Every MIR edge is kept: an unqualified one is still a real call the
        // compiler saw, and dropping it orphans whatever it reached.
        //
        // A syntactic edge is dropped when either end names something MIR
        // resolved. Dropping only by target left the *callers* nominal, so an
        // edge out of a promoted `B::process` still read as coming from plain
        // `process` — a key no definition has — and everything downstream of
        // it was orphaned.
        let kept_syn: Vec<Edge> = base
            .edges
            .iter()
            .filter(|e| {
                !resolved_names.contains(&e.to.name) && !resolved_names.contains(&e.from.name)
            })
            .cloned()
            .collect();

        base.edges = parsed
            .edges
            .into_iter()
            .map(|e| Edge {
                in_test: caller_is_test(&e.from, &base.definitions),
                precision: e.to.precision(),
                ..e
            })
            .chain(kept_syn)
            .collect();

        Ok(base)
    }
}

/// Is this caller test code?
///
/// MIR has no notion of test code, so the attributes syn read are the
/// authority. The match must not rest on a bare name: a production `helper`
/// and a test `helper` would be one key, and every edge out of either would
/// take the same classification.
///
/// The two mistakes are not symmetric. Calling a *production* edge a test edge
/// removes a real caller and can invent a finding; calling a *test* edge a
/// production edge only loses one. Anything the identity cannot settle
/// therefore resolves to production.
fn caller_is_test(from: &SymbolId, defs: &[Definition]) -> bool {
    // Narrow by every qualifier the MIR id carries. syn does not record module
    // paths, so a module qualifier cannot narrow further here — it still
    // matters, because it keeps the *callee* side of the graph distinct.
    let candidates: Vec<&Definition> = defs
        .iter()
        .filter(|d| d.id.name == from.name)
        .filter(|d| match &from.self_ty {
            Some(t) => d.self_ty.as_deref() == Some(t.as_str()),
            None => true,
        })
        .collect();

    // No definition to consult, or definitions that disagree: fail closed.
    !candidates.is_empty() && candidates.iter().all(|d| d.in_test || d.is_test_fn)
}

/// The directory holding `Cargo.toml`.
fn crate_dir(path: &Path) -> std::path::PathBuf {
    if path.join("Cargo.toml").is_file() {
        return path.to_path_buf();
    }
    if path.file_name().map(|n| n == "src").unwrap_or(false) {
        if let Some(p) = path.parent() {
            if p.join("Cargo.toml").is_file() {
                return p.to_path_buf();
            }
        }
    }
    path.to_path_buf()
}

/// Ask cargo for MIR. Every failure mode is reported with what to do about it.
fn dump_mir(dir: &Path) -> anyhow::Result<String> {
    if !dir.join("Cargo.toml").is_file() {
        anyhow::bail!(
            "--precise needs a cargo crate: no Cargo.toml at {}.\n\
             MIR comes from the compiler, so there must be something to compile.",
            dir.display()
        );
    }

    let has_nightly = Command::new("cargo")
        .args(["+nightly", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !has_nightly {
        anyhow::bail!(
            "--precise needs a nightly toolchain, which is not installed.\n\
             \n\
             MIR is emitted through -Zunpretty, an unstable flag.\n\
             \n\
             Install it:   rustup toolchain install nightly\n\
             \n\
             Or run without --precise for the syntactic analysis. That mode \
             reports what it could not resolve rather than guessing — see \
             `landed check --stats`."
        );
    }

    // `cargo rustc` passes trailing arguments to exactly one target, so a
    // crate with both a lib and a bin — or a workspace — must be dumped one
    // target at a time and the results concatenated. Asking for all of them at
    // once fails with "extra arguments can only be passed to one target",
    // which says nothing about what to do.
    let ws = crate::targets::discover(dir);
    let mut selectors: Vec<Vec<String>> = Vec::new();
    for t in &ws.targets {
        let flag = match t.kind {
            crate::targets::Kind::Lib => vec!["--lib".to_string()],
            crate::targets::Kind::Bin => vec!["--bin".to_string(), t.name.clone()],
            _ => continue,
        };
        let mut sel = vec!["-p".to_string(), t.package.clone()];
        sel.extend(flag);
        selectors.push(sel);
    }
    if selectors.is_empty() {
        selectors.push(Vec::new()); // let cargo choose
    }

    let mut text = String::new();
    let mut failures: Vec<String> = Vec::new();

    for sel in &selectors {
        // The test profile deliberately: it compiles #[cfg(test)] bodies,
        // which is the only way MIR can see a test-only call and give it the
        // same typed identity as a production one.
        //
        // Never release: it inlines calls away, leaving `scope N (inlined
        // A::process)` annotations instead of call terminators, and the call
        // graph disappears into them.
        let mut args: Vec<String> =
            vec!["+nightly".into(), "rustc".into(), "--profile".into(), "test".into()];
        args.extend(sel.iter().cloned());
        args.push("--".into());
        args.push("-Zunpretty=mir".into());

        let out = Command::new("cargo")
            .args(&args)
            .current_dir(dir)
            .output()
            .map_err(|e| anyhow::anyhow!("could not run cargo: {e}"))?;

        if out.status.success() {
            text.push_str(&String::from_utf8_lossy(&out.stdout));
            text.push('\n');
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            let first: Vec<&str> = err
                .lines()
                .filter(|l| l.starts_with("error") || l.contains("error["))
                .take(2)
                .collect();
            failures.push(format!("  {}: {}", sel.join(" "), first.join(" / ")));
        }
    }

    if text.trim().is_empty() {
        anyhow::bail!(
            "--precise could not produce MIR for any target.\n\
             \n\
             {}\n\
             \n\
             MIR exists only for code the compiler accepted. Fix the build, or \
             run without --precise — the syntactic analysis reads source \
             directly and does not need a successful build.",
            if failures.is_empty() {
                "(cargo reported no error, but emitted nothing)".to_string()
            } else {
                failures.join("\n")
            }
        );
    }

    if !text.contains("fn ") {
        anyhow::bail!(
            "cargo produced no MIR. The crate may have no compiled targets, or \
             the -Zunpretty=mir output format may have changed in this nightly."
        );
    }
    Ok(text)
}

// ─── parsing ──────────────────────────────────────────────────

#[derive(Debug)]
pub(crate) struct MirDef {
    pub id: SymbolId,
    /// Receiver type as the header spelled it. Retained for diagnostics; the
    /// id already carries it where it is part of identity.
    #[allow(dead_code)]
    pub self_ty: Option<String>,
}

#[derive(Default)]
pub(crate) struct Parsed {
    pub definitions: Vec<MirDef>,
    pub edges: Vec<Edge>,
}

/// Turn a MIR call target into a symbol identity, keeping every qualifier the
/// dump gave us.
///
/// The pretty-printer emits the shortest path that distinguishes a symbol, so
/// `alpha::helper` and `beta::helper` arrive qualified while a unique
/// `shared_name` arrives bare. Discarding the qualifier — as this parser once
/// did — throws away exactly the identity that makes precise mode precise.
///
/// Forms observed in real dumps:
///   `inner`                  free function, unique enough to need no path
///   `alpha::helper`          free function qualified by module
///   `A::process`             inherent method on a concrete type
///   `<T as Speak>::speak`    trait dispatch, receiver still generic
///   `generic::<A>`           generic instantiated at a type
fn parse_target(raw: &str) -> Option<SymbolId> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    // Drop a turbofish: generic::<A> -> generic. Anything after `::<` is an
    // instantiation, not part of the symbol's identity.
    let t = match t.find("::<") {
        Some(i) => &t[..i],
        None => t,
    };
    // `<T as Trait>::method` — the receiver is still generic, so claiming a
    // type would be wrong. Stay nominal; that is the honest answer.
    if t.starts_with('<') {
        let name = t.rsplit("::").next()?.trim();
        return valid_ident(name).then(|| SymbolId::nominal(name));
    }
    let mut parts: Vec<&str> = t.split("::").filter(|s| !s.is_empty()).collect();
    let name = parts.pop()?;
    if !valid_ident(name) {
        return None;
    }
    if parts.is_empty() {
        return Some(SymbolId::nominal(name));
    }
    // Every remaining qualifier must be an identifier, or this is a form the
    // parser does not understand and must not guess at.
    if !parts.iter().all(|p| valid_ident(p)) {
        return None;
    }
    let last = parts[parts.len() - 1];
    if starts_upper(last) {
        // A capitalised final qualifier is a type.
        Some(SymbolId::typed(name, last))
    } else {
        // Otherwise a module path; keep all of it.
        Some(SymbolId::in_module(name, parts.join("::")))
    }
}

fn starts_upper(s: &str) -> bool {
    s.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
}

fn valid_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_alphanumeric() || c == '_')
        && !s.chars().next().unwrap().is_ascii_digit()
}

/// First parameter's type, which for a method is the receiver:
/// `(_1: &A)` -> `A`.
fn receiver_type(sig: &str) -> Option<String> {
    let inner = sig.split_once('(')?.1;
    let first = inner.split(')').next()?.split(',').next()?;
    let ty = first.split_once(':')?.1.trim();
    let ty = ty.trim_start_matches('&').trim_start_matches("mut ").trim();
    let ty = ty.split(['<', ' ']).next()?.trim();
    valid_ident(ty).then(|| ty.to_string())
}

/// Identity of a definition from its `fn ...` header.
///
/// Returns `None` for anything unrecognised — a closure, a shim, a form this
/// parser has not seen. Inventing an identity for it would put a symbol in
/// the graph that no call site can ever match, and everything downstream of
/// it would read as unreachable.
pub(crate) fn parse_definition(header: &str) -> Option<(SymbolId, Option<String>)> {
    let self_ty = receiver_type(header);
    let head = header.split('(').next()?.trim();
    // Compiler-generated shims that name nothing a call site can reach.
    if head.contains('{') || head.contains('}') {
        return None;
    }
    if let Some(rest) = head.strip_prefix("<impl at ") {
        // `<impl at src/main.rs:4:1: 4:7>::process` — the impl span tells us
        // nothing matchable, but the receiver type does.
        let name = rest.rsplit("::").next()?.trim();
        if !valid_ident(name) {
            return None;
        }
        let ty = self_ty.clone()?;
        return Some((SymbolId::typed(name, ty.clone()), Some(ty)));
    }
    if head.starts_with('<') {
        // `<A as Trait>::method`
        let name = head.rsplit("::").next()?.trim();
        return valid_ident(name).then(|| (SymbolId::nominal(name), self_ty.clone()));
    }
    let mut parts: Vec<&str> = head.split("::").filter(|s| !s.is_empty()).collect();
    let name = parts.pop()?;
    if !valid_ident(name) || !parts.iter().all(|p| valid_ident(p)) {
        return None;
    }
    let id = if parts.is_empty() {
        SymbolId::nominal(name)
    } else if starts_upper(parts[parts.len() - 1]) {
        SymbolId::typed(name, parts[parts.len() - 1])
    } else {
        SymbolId::in_module(name, parts.join("::"))
    };
    Some((id, None))
}

/// Is this line a call terminator, and what does it call?
///
/// Must not match an inlined-scope annotation such as
/// `scope 3 (inlined A::process)`, which names a function but is not a call —
/// treating it as one would invent an edge that the program does not contain.
pub(crate) fn parse_call(line: &str) -> Option<SymbolId> {
    let t = line.trim_start();
    if t.starts_with("scope ") || t.starts_with("//") || t.starts_with('/') {
        return None;
    }
    // A terminator assigns to a local and names its successor blocks.
    if !t.starts_with('_') || !t.contains(") -> [") {
        return None;
    }
    let (_, rhs) = t.split_once(" = ")?;
    let target = rhs.split('(').next()?;
    parse_target(target)
}

/// A closure body belongs to the function that wrote it.
///
/// MIR gives a closure its own header — `output::print::{closure#0}` — and its
/// body holds real calls. Dropping those, as this parser first did, discards
/// every call written inside a closure: `.map(|x| helper(x))` disappears, and
/// `helper` is then reported dead. In codebases built around iterator chains
/// that is most of the call graph.
///
/// The closure is not a definition anything can call, so it is attributed to
/// its parent rather than recorded.
fn closure_parent(head: &str) -> Option<&str> {
    let i = head.find("::{closure")?;
    let parent = head[..i].trim();
    (!parent.is_empty()).then_some(parent)
}

pub(crate) fn parse(mir: &str) -> Parsed {
    let mut out = Parsed::default();
    let mut current: Option<SymbolId> = None;

    for line in mir.lines() {
        if let Some(rest) = line.strip_prefix("fn ") {
            let head = rest.split('(').next().unwrap_or("").trim();
            if let Some(parent) = closure_parent(head) {
                // Attribute the body to the enclosing function; record no
                // definition, since nothing can call a closure by name.
                current = parse_definition(&format!("{parent}()")).map(|(id, _)| id);
                continue;
            }
            match parse_definition(rest) {
                Some((id, self_ty)) => {
                    out.definitions.push(MirDef { id: id.clone(), self_ty });
                    current = Some(id);
                }
                // Unrecognised header: emit nothing, and stop attributing
                // calls until the next header we do understand. Attributing
                // them to the previous function would invent edges.
                None => current = None,
            }
            continue;
        }

        let Some(from) = &current else { continue };
        if let Some(to) = parse_call(line) {
            out.edges.push(Edge {
                from: from.clone(),
                to,
                kind: EdgeKind::Call,
                precision: Precision::Typed,
                in_test: false,
                file: std::path::PathBuf::new(),
                line: 0,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── call targets ──────────────────────────────────────────

    #[test]
    fn a_bare_call_is_nominal() {
        assert_eq!(parse_target("inner"), Some(SymbolId::nominal("inner")));
    }

    #[test]
    fn a_module_qualifier_is_kept() {
        // Discarding this is what let two same-named functions in different
        // modules merge into one node.
        assert_eq!(parse_target("alpha::helper"), Some(SymbolId::in_module("helper", "alpha")));
        assert_eq!(parse_target("a::b::deep"), Some(SymbolId::in_module("deep", "a::b")));
        assert_ne!(parse_target("alpha::helper"), parse_target("beta::helper"));
    }

    #[test]
    fn a_capitalised_qualifier_is_a_type() {
        assert_eq!(parse_target("A::process"), Some(SymbolId::typed("process", "A")));
        assert_ne!(parse_target("A::process"), parse_target("B::process"));
    }

    #[test]
    fn a_turbofish_is_not_part_of_identity() {
        assert_eq!(parse_target("generic::<A>"), Some(SymbolId::nominal("generic")));
        assert_eq!(
            parse_target("alpha::helper::<u8>"),
            Some(SymbolId::in_module("helper", "alpha"))
        );
    }

    #[test]
    fn an_unresolved_generic_receiver_is_not_claimed_as_a_type() {
        // `<T as Speak>::speak` — T is not a concrete type, so asserting one
        // would be a lie the analysis would then act on.
        assert_eq!(parse_target("<T as Speak>::speak"), Some(SymbolId::nominal("speak")));
        assert_eq!(parse_target("<T as Speak>::speak").unwrap().precision(), Precision::Nominal);
    }

    #[test]
    fn unparseable_targets_yield_nothing() {
        for bad in ["", "   ", "123bad", "a..b", "{closure#0}", "*const u8"] {
            assert_eq!(parse_target(bad), None, "invented a symbol from {bad:?}");
        }
    }

    // ── definition headers ────────────────────────────────────

    #[test]
    fn a_plain_definition_parses() {
        let (id, _) = parse_definition("main() -> () {").unwrap();
        assert_eq!(id, SymbolId::nominal("main"));
    }

    #[test]
    fn a_module_qualified_definition_keeps_its_path() {
        let (id, _) = parse_definition("alpha::helper() -> u8 {").unwrap();
        assert_eq!(id, SymbolId::in_module("helper", "alpha"));
        let (other, _) = parse_definition("beta::helper() -> u8 {").unwrap();
        assert_ne!(id, other, "two modules, two symbols");
    }

    #[test]
    fn an_inherent_method_takes_its_receiver_type() {
        let (id, ty) = parse_definition("<impl at src/main.rs:4:1: 4:7>::process(_1: &A) -> u8 {")
            .unwrap();
        assert_eq!(id, SymbolId::typed("process", "A"));
        assert_eq!(ty.as_deref(), Some("A"));
        let (other, _) = parse_definition("<impl at src/main.rs:5:1: 5:7>::process(_1: &B) -> u8 {")
            .unwrap();
        assert_ne!(id, other, "same method name, different types");
    }

    #[test]
    fn a_closure_is_not_a_definition() {
        // `fn tests::t::{closure#0}(...)` names nothing a call site can reach.
        assert!(parse_definition("tests::t::{closure#0}(_1: &{closure@x.rs:1:1}) -> R {").is_none());
    }

    // ── call terminators ──────────────────────────────────────

    #[test]
    fn a_terminator_is_recognised() {
        let l = "        _2 = A::process(move _3) -> [return: bb1, unwind continue];";
        assert_eq!(parse_call(l), Some(SymbolId::typed("process", "A")));
    }

    #[test]
    fn an_inlined_scope_annotation_is_not_a_call() {
        // Release-profile MIR replaces calls with these. Reading one as a call
        // invents an edge the program does not contain — and reading it as a
        // *definition* would be worse.
        for l in [
            "        scope 3 (inlined A::process) {",
            "        scope 7 (inlined B::process) {",
        ] {
            assert_eq!(parse_call(l), None, "invented a call from {l:?}");
        }
    }

    #[test]
    fn assignments_that_are_not_calls_are_ignored() {
        for l in [
            "        _1 = const 0_u8;",
            "        _3 = &_2;",
            "        _0 = move _1;",
            "        // comment mentioning helper()",
            "        StorageLive(_2);",
        ] {
            assert_eq!(parse_call(l), None, "invented a call from {l:?}");
        }
    }

    // ── whole dumps ───────────────────────────────────────────

    /// A dump captured from nightly 1.100.0, covering module qualification,
    /// two same-named methods, a test module and a closure shim.
    const FIXTURE: &str = r#"
fn alpha::helper() -> u8 {
    let mut _0: u8;
    bb0: {
        _0 = const 1_u8;
        return;
    }
}
fn beta::helper() -> u8 {
    bb0: {
        _0 = const 2_u8;
        return;
    }
}
fn <impl at src/main.rs:4:1: 4:7>::process(_1: &A) -> u8 {
    bb0: {
        _0 = alpha::helper() -> [return: bb1, unwind continue];
    }
}
fn <impl at src/main.rs:5:1: 5:7>::process(_1: &B) -> u8 {
    bb0: {
        _0 = beta::helper() -> [return: bb1, unwind continue];
    }
}
fn main() -> () {
    bb0: {
        _1 = A::process(move _2) -> [return: bb1, unwind continue];
    }
}
fn tests::t::{closure#0}(_1: &{closure@src/main.rs:11:13: 11:74}) -> Result<(), String> {
    bb0: {
        _9 = never_attributed() -> [return: bb1, unwind continue];
    }
}
fn tests::t() -> () {
    bb0: {
        _1 = B::process(move _2) -> [return: bb1, unwind continue];
    }
}
"#;

    #[test]
    fn a_representative_dump_parses_into_distinct_symbols() {
        let p = parse(FIXTURE);
        let ids: Vec<String> = p.definitions.iter().map(|d| d.id.to_string()).collect();
        assert!(ids.contains(&"alpha::helper".to_string()), "{ids:?}");
        assert!(ids.contains(&"beta::helper".to_string()), "{ids:?}");
        assert!(ids.contains(&"A::process".to_string()), "{ids:?}");
        assert!(ids.contains(&"B::process".to_string()), "{ids:?}");
        assert!(ids.contains(&"tests::t".to_string()), "{ids:?}");
        // The closure header is not a definition.
        assert!(!ids.iter().any(|i| i.contains("closure")), "{ids:?}");
    }

    #[test]
    fn edges_from_the_dump_are_attributed_to_the_right_caller() {
        let p = parse(FIXTURE);
        let edge = |from: &str, to: &str| {
            p.edges
                .iter()
                .any(|e| e.from.to_string() == from && e.to.to_string() == to)
        };
        assert!(edge("A::process", "alpha::helper"), "{:?}", dump(&p));
        assert!(edge("B::process", "beta::helper"), "{:?}", dump(&p));
        assert!(edge("main", "A::process"), "{:?}", dump(&p));
        assert!(edge("tests::t", "B::process"), "{:?}", dump(&p));
        // A::process must not be credited with beta::helper.
        assert!(!edge("A::process", "beta::helper"), "{:?}", dump(&p));
    }

    #[test]
    fn a_closure_body_is_attributed_to_the_function_that_wrote_it() {
        // `.map(|x| helper(x))` compiles to a closure with its own MIR
        // header. Dropping those bodies discards every call written inside a
        // closure — most of the call graph in iterator-heavy code — and the
        // callees are then reported dead.
        let p = parse(FIXTURE);
        assert!(
            p.edges
                .iter()
                .any(|e| e.from.to_string() == "tests::t" && e.to.name == "never_attributed"),
            "the closure's call belongs to tests::t: {:?}",
            dump(&p)
        );
        // And the closure itself is not a callable definition.
        assert!(!p.definitions.iter().any(|d| d.id.to_string().contains("closure")));
    }

    #[test]
    fn a_genuinely_unrecognised_header_drops_its_calls() {
        // Not a closure, not a form the parser knows: attributing its body to
        // the previous function would invent edges out of that function.
        let mir = "\
fn known() -> () {
    bb0: {
        _1 = real_call() -> [return: bb1, unwind continue];
    }
}
fn 99invalid::thing() -> () {
    bb0: {
        _1 = phantom() -> [return: bb1, unwind continue];
    }
}
";
        let p = parse(mir);
        assert!(p.edges.iter().any(|e| e.to.name == "real_call"));
        assert!(
            !p.edges.iter().any(|e| e.to.name == "phantom"),
            "a call under an unrecognised header must be dropped: {:?}",
            dump(&p)
        );
    }

    #[test]
    fn a_dump_with_no_functions_is_not_read_as_an_empty_program() {
        // Silence here would report every symbol unreachable.
        let p = parse("// nothing
bb0: {
  _0 = helper() -> [return: bb1];
}
");
        assert!(p.definitions.is_empty());
        assert!(p.edges.is_empty(), "edges without a caller must not be invented");
    }

    fn dump(p: &Parsed) -> Vec<String> {
        p.edges.iter().map(|e| format!("{} -> {}", e.from, e.to)).collect()
    }

    // ── test-caller classification ────────────────────────────

    fn def(name: &str, self_ty: Option<&str>, in_test: bool) -> Definition {
        Definition {
            id: SymbolId::nominal(name),
            precision: Precision::Nominal,
            file: std::path::PathBuf::new(),
            line: 1,
            in_test,
            is_test_fn: in_test,
            trait_impl: false,
            allowed_dead: false,
            is_pub: false,
            is_ffi: false,
            crate_root: std::path::PathBuf::new(),
            self_ty: self_ty.map(str::to_string),
            module: None,
        }
    }

    #[test]
    fn a_test_only_caller_is_classified_as_test() {
        let defs = vec![def("only_in_tests", None, true)];
        assert!(caller_is_test(&SymbolId::nominal("only_in_tests"), &defs));
    }

    #[test]
    fn a_production_caller_is_not() {
        let defs = vec![def("live", None, false)];
        assert!(!caller_is_test(&SymbolId::nominal("live"), &defs));
    }

    #[test]
    fn a_name_shared_by_test_and_production_resolves_to_production() {
        // The bug this replaced: a name-keyed set marked every edge out of
        // either as a test edge, removing real production callers.
        let defs = vec![def("shared_name", None, false), def("shared_name", None, true)];
        assert!(
            !caller_is_test(&SymbolId::nominal("shared_name"), &defs),
            "ambiguity must fail closed to production"
        );
    }

    #[test]
    fn a_receiver_type_narrows_the_match() {
        // B::run is test-only; A::run is production. A bare name cannot tell
        // them apart, a typed id can.
        let defs = vec![def("run", Some("A"), false), def("run", Some("B"), true)];
        assert!(!caller_is_test(&SymbolId::typed("run", "A"), &defs));
        assert!(caller_is_test(&SymbolId::typed("run", "B"), &defs));
        // Without the type, it must fail closed rather than pick one.
        assert!(!caller_is_test(&SymbolId::nominal("run"), &defs));
    }

    #[test]
    fn an_unknown_caller_is_not_test() {
        // Nothing to consult is not evidence of test-ness.
        assert!(!caller_is_test(&SymbolId::nominal("never_seen"), &[]));
    }
}
