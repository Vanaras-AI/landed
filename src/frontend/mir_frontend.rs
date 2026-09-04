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
use std::collections::HashMap;
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
        // line for a free function. The two are combined rather than one
        // replacing the other.
        let mut base = super::SynFrontend.extract(root)?;
        let parsed = parse(&mir);

        // Promote a definition's id to typed when MIR agrees a method of that
        // name exists on that type. Metadata stays as syn recorded it.
        let typed_defs: std::collections::HashSet<(String, String)> = parsed
            .definitions
            .iter()
            .filter_map(|d| d.self_ty.clone().map(|t| (d.name.clone(), t)))
            .collect();

        for d in &mut base.definitions {
            if let Some(ty) = &d.self_ty {
                if typed_defs.contains(&(d.id.name.clone(), ty.clone())) {
                    d.id = SymbolId::typed(d.id.name.clone(), ty.clone());
                    d.precision = Precision::Typed;
                }
            }
        }

        // Edges come from MIR entirely. A syntactic edge cannot name its
        // target's type, so keeping any would reintroduce exactly the
        // ambiguity this mode exists to remove.
        //
        // MIR carries no notion of test code, so the caller decides: an edge
        // out of a function syn recorded as test code is a test edge. syn is
        // the authority on that, having read the attributes.
        let test_callers: std::collections::HashSet<&str> = base
            .definitions
            .iter()
            .filter(|d| d.in_test || d.is_test_fn)
            .map(|d| d.id.name.as_str())
            .collect();

        base.edges = parsed
            .edges
            .into_iter()
            .map(|e| Edge {
                in_test: test_callers.contains(e.from.name.as_str()),
                precision: if e.to.self_ty.is_some() {
                    Precision::Typed
                } else {
                    Precision::Nominal
                },
                ..e
            })
            .collect();

        Ok(base)
    }
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
struct MirDef {
    name: String,
    self_ty: Option<String>,
}

#[derive(Default)]
struct Parsed {
    definitions: Vec<MirDef>,
    edges: Vec<Edge>,
}

/// Pull the last path segment out of a MIR call target.
///
/// Handles the forms the dump actually produces:
///   `inner`                  free function
///   `A::process`             inherent method on a concrete type
///   `<T as Speak>::speak`    trait dispatch, receiver still generic
///   `generic::<A>`           generic instantiated at a type
fn parse_target(raw: &str) -> Option<SymbolId> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    // Drop turbofish: generic::<A> -> generic
    let t = match t.find("::<") {
        Some(i) => &t[..i],
        None => t,
    };
    // <T as Trait>::method -> trait name is not a concrete receiver, so the
    // method stays nominal: an unresolved generic is exactly the case where
    // claiming a type would be wrong.
    if let Some(rest) = t.strip_prefix('<') {
        let name = rest.rsplit("::").next()?.trim();
        return valid_ident(name).then(|| SymbolId::nominal(name));
    }
    let mut parts: Vec<&str> = t.split("::").filter(|s| !s.is_empty()).collect();
    let name = parts.pop()?;
    if !valid_ident(name) {
        return None;
    }
    match parts.pop() {
        // A capitalised penultimate segment is a type; a lowercase one is a
        // module, and a module path is not an identity MIR can promise.
        Some(ty) if ty.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) => {
            Some(SymbolId::typed(name, ty))
        }
        _ => Some(SymbolId::nominal(name)),
    }
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

fn parse(mir: &str) -> Parsed {
    let mut out = Parsed::default();
    let mut current: Option<SymbolId> = None;

    for line in mir.lines() {
        if let Some(rest) = line.strip_prefix("fn ") {
            let self_ty = receiver_type(rest);
            let head = rest.split('(').next().unwrap_or("").trim();
            // `<impl at src/main.rs:3:1: 3:7>::process`
            let name = head.rsplit("::").next().unwrap_or(head).trim();
            if !valid_ident(name) {
                current = None;
                continue;
            }
            // Only a method gets a typed id; the receiver type of a free
            // function is just its first argument.
            let is_method = head.contains("::");
            let id = match (&self_ty, is_method) {
                (Some(t), true) => SymbolId::typed(name, t.clone()),
                _ => SymbolId::nominal(name),
            };
            out.definitions.push(MirDef {
                name: name.to_string(),
                self_ty: if is_method { self_ty } else { None },
            });
            current = Some(id);
            continue;
        }

        // `        _2 = A::process(move _3) -> [return: bb1, ...];`
        let Some(from) = &current else { continue };
        let t = line.trim_start();
        if !t.starts_with('_') || !t.contains(") -> [") {
            continue;
        }
        let Some((_, rhs)) = t.split_once(" = ") else { continue };
        let Some(target_raw) = rhs.split('(').next() else { continue };
        if let Some(to) = parse_target(target_raw) {
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

/// How much ambiguity this frontend removed, for `--stats`.
pub fn resolution_gain(before: &HashMap<String, usize>, after: &HashMap<String, usize>) -> f64 {
    let amb = |m: &HashMap<String, usize>| m.values().filter(|&&c| c > 1).sum::<usize>() as f64;
    let (b, a) = (amb(before), amb(after));
    if b == 0.0 {
        0.0
    } else {
        (b - a) / b * 100.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_parse() {
        assert_eq!(parse_target("inner"), Some(SymbolId::nominal("inner")));
        assert_eq!(parse_target("A::process"), Some(SymbolId::typed("process", "A")));
        assert_eq!(parse_target("generic::<A>"), Some(SymbolId::nominal("generic")));
        // An unresolved generic receiver must not be claimed as a type.
        assert_eq!(parse_target("<T as Speak>::speak"), Some(SymbolId::nominal("speak")));
        // A module path is not an identity MIR can promise.
        assert_eq!(parse_target("deep::nested"), Some(SymbolId::nominal("nested")));
    }

    #[test]
    fn receivers_parse() {
        assert_eq!(receiver_type("<impl at x.rs:1:1>::process(_1: &A) -> u8 {"), Some("A".into()));
        assert_eq!(receiver_type("process(_1: &mut B) -> u8 {"), Some("B".into()));
        assert_eq!(receiver_type("main() -> () {"), None);
    }
}
