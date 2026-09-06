//! Analysis over the frontend-independent IR.
//!
//! Nothing here knows how a definition or an edge was obtained — only how far
//! it can be trusted. Adding a frontend must not require editing this file.

use crate::ir::{Definition, Extract, Precision};
use crate::targets::Workspace;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Kept as an alias so consumers that spoke of `FnDef` still compile; the IR
/// name is the real one.
pub type FnDef = Definition;

/// Call sites for one symbol, split by context.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct CallSites {
    pub prod: usize,
    pub test: usize,
    /// Up to 3 example locations, for the report.
    pub examples: Vec<String>,
}

/// A crate, analysed.
pub struct Scan {
    pub defs: Vec<Definition>,
    /// Aggregate call counts per symbol, derived from the edges.
    pub calls: HashMap<String, CallSites>,
    /// Adjacency: caller name -> names it invokes.
    pub edges: HashMap<String, HashSet<String>>,
    /// Every crate `src/` dir that was read.
    pub crate_roots: Vec<PathBuf>,
    /// Crate layout as cargo reports it. Empty for non-Rust projects.
    pub workspace: Workspace,
    /// What the project says about itself, in whatever language it is written:
    /// whether anything here runs on its own, and what its entry points are.
    /// The analysis asks the project once, and never asks a build tool.
    pub is_application: bool,
    pub project_entry_points: Vec<String>,
    /// Files a host loads. Whatever they export is called from outside.
    pub entry_files: Vec<PathBuf>,
    /// How those answers were reached, for `--explain` and for anyone who
    /// thinks the analysis picked wrong.
    pub project_description: String,
    /// Developer-declared roots and ignores.
    pub config: crate::config::Config,
    /// Names re-exported at a crate root.
    pub reexported: HashSet<String>,
    /// Best precision the frontend that produced this could offer.
    pub precision: Precision,
    /// How many findings the nominal tier reported, when a more precise tier
    /// is running. A precise tier should mostly *remove* findings by settling
    /// ambiguity; if it adds many, the likelier cause is a call form its
    /// parser did not read than a sudden abundance of dead code, and the
    /// report says so rather than presenting the number as fact.
    pub nominal_findings: Option<usize>,
}

impl Default for Scan {
    fn default() -> Self {
        Self {
            defs: Vec::new(),
            calls: HashMap::new(),
            edges: HashMap::new(),
            crate_roots: Vec::new(),
            workspace: Workspace::default(),
            config: crate::config::Config::default(),
            reexported: HashSet::new(),
            precision: Precision::Nominal,
            nominal_findings: None,
            is_application: false,
            project_entry_points: Vec::new(),
            entry_files: Vec::new(),
            project_description: String::new(),
        }
    }
}

impl Scan {
    /// Fold a frontend's output into the analysable form.
    ///
    /// Edge kinds are honoured here, once, rather than at each use: a macro
    /// token match may raise a production count — which can only ever remove
    /// a finding — but never a test count, which could create one.
    pub fn from_extract(ex: Extract, precision: Precision) -> Self {
        let mut calls: HashMap<String, CallSites> = HashMap::new();
        let mut edges: HashMap<String, HashSet<String>> = HashMap::new();

        // An edge whose target the frontend could not qualify says only "some
        // function of this name was called". If definitions are keyed more
        // precisely than that — as they are once MIR has promoted them — such
        // an edge would reach none of them, and every one would be stranded
        // and reported dead.
        //
        // So an unqualified target credits every definition sharing its name.
        // That over-approximates liveness, which removes findings; the
        // alternative invents them, and this analysis is built to fail the
        // first way.
        let mut by_name: HashMap<&str, Vec<String>> = HashMap::new();
        for d in &ex.definitions {
            by_name.entry(d.name()).or_default().push(d.key());
        }
        let known: HashSet<String> = ex.definitions.iter().map(|d| d.key()).collect();
        let targets_of = |to: &crate::ir::SymbolId| -> Vec<String> {
            let exact = to.to_string();
            // A qualifier only helps if it names a definition we actually
            // hold. A frontend may qualify with something the definition side
            // never saw — a crate name on a cross-target call, for one — and
            // an edge that matches nothing strands its callee.
            if to.is_qualified() && known.contains(&exact) {
                return vec![exact];
            }
            match by_name.get(to.name.as_str()) {
                Some(keys) if !keys.is_empty() => keys.clone(),
                _ => vec![exact],
            }
        };

        for e in &ex.edges {
            let from = e.from.to_string();
            for target in targets_of(&e.to) {
                edges
                    .entry(from.clone())
                    .or_default()
                    .insert(target.clone());

                let entry = calls.entry(target).or_default();
                if e.in_test {
                    if e.kind.can_create_finding() {
                        entry.test += 1;
                        // A frontend that resolves calls precisely may not
                        // know where they were written. An absent location is
                        // omitted rather than printed as ":0".
                        if entry.examples.len() < 3 && e.line > 0 {
                            entry
                                .examples
                                .push(format!("{}:{}", e.file.display(), e.line));
                        }
                    }
                } else {
                    entry.prod += 1;
                }
            }
        }

        Scan {
            defs: ex.definitions,
            calls,
            edges,
            crate_roots: ex.crate_roots,
            reexported: ex.reexported,
            precision,
            ..Default::default()
        }
    }
}

/// Analyse a crate with the default (syntactic) tier.
pub fn scan_crate(root: &Path) -> anyhow::Result<Scan> {
    scan_crate_with(root, crate::frontend::Tier::Default)
}

/// Analyse a crate with a chosen tier.
pub fn scan_crate_with(root: &Path, tier: crate::frontend::Tier) -> anyhow::Result<Scan> {
    scan_crate_as(root, tier, None)
}

/// Analyse a crate with a chosen tier, and a language stated rather than
/// detected. Detection reads manifests first and file counts second, which is
/// right for a project and wrong for a polyglot repository.
pub fn scan_crate_as(
    root: &Path,
    tier: crate::frontend::Tier,
    lang: Option<crate::lang::Language>,
) -> anyhow::Result<Scan> {
    let ex = crate::frontend::extract_as(tier, root, lang)?;
    let mut scan = Scan::from_extract(ex, crate::frontend::precision(tier));
    scan.config = crate::config::Config::load(root).unwrap_or_default();
    scan.workspace = crate::targets::discover(root);

    // The project answers for itself, in whatever language it is written.
    let project = crate::project::detect_as(root, lang);
    scan.is_application = project.is_application();
    scan.project_entry_points = project.entry_points();
    scan.entry_files = project.entry_files();
    scan.project_description = project.describe();

    if tier != crate::frontend::Tier::Default {
        if let Ok(base) = scan_crate(root) {
            scan.nominal_findings = Some(never_run_graph(&base).len());
        }
    }
    Ok(scan)
}

/// Which directories the default frontend would read. Retained for `--explain`.
pub fn resolve_roots(path: &Path) -> Vec<PathBuf> {
    crate::frontend::syn_frontend::resolve_roots(path)
}

/// Names that are always reachable or conventionally unreferenced.
const ALWAYS_LIVE: &[&str] = &[
    "main",
    "new",
    "default",
    "fmt",
    "from",
    "from_str",
    "try_from",
    "drop",
    "clone",
    "next",
    "poll",
    "deref",
    "deref_mut",
    "eq",
    "ne",
    "hash",
    "cmp",
    "partial_cmp",
    "serialize",
    "deserialize",
    "into",
    "as_ref",
    "borrow",
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
        if ALWAYS_LIVE.contains(&d.name()) || scan.config.is_ignored(d.name()) {
            continue;
        }
        // Re-exported from the crate root: it is public API, and its consumers
        // are outside this tree.
        if scan.reexported.contains(d.name()) {
            continue;
        }
        // A name defined more than once is ambiguous under name-based matching;
        // skip it rather than risk a false positive.
        if scan
            .defs
            .iter()
            .filter(|o| o.key() == d.key() && !o.in_test)
            .count()
            > 1
        {
            continue;
        }
        if let Some(c) = scan.calls.get(&d.key()) {
            if c.prod == 0 && c.test > 0 {
                out.push(Finding {
                    name: d.key(),
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
    out.sort_by(|a, b| b.test_calls.cmp(&a.test_calls).then(a.name.cmp(&b.name)));
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

    // Is this an application or a library?
    //
    // An application has something that runs on its own. Its internal library
    // surface is not an entry point — it is reached through that binary — so
    // code nothing runs is genuinely dead.
    //
    // A project with nothing runnable is a library. Its consumers are other
    // people's code, which we cannot see, so its whole public API must be
    // treated as reachable or the entire codebase would be accused. (Observed
    // before this rule: a 120-fn library reported 51% dead, all false.)
    //
    // Every language answers differently, and none of them is asked here —
    // the project was asked, once, when the scan was built.
    let is_application = scan.is_application;

    // Entry points the language declares regardless of who calls them.
    for name in &scan.project_entry_points {
        roots.insert(name.clone());
    }

    // What a host loads, it also calls. An entry module's exports are entered
    // from outside the repository, so nothing here will ever be seen calling
    // them.
    if !scan.entry_files.is_empty() {
        for d in &scan.defs {
            if d.is_pub && !d.in_test && scan.entry_files.contains(&d.file) {
                roots.insert(d.key());
            }
        }
    }

    for d in &scan.defs {
        if d.in_test || d.is_test_fn {
            continue;
        }
        // A root the developer declared in landed.toml outranks every
        // heuristic: they know how their program is entered, and the analyzer
        // cannot see through a task spawn or a handler registry.
        let declared = scan.config.is_root(d.name());
        let externally_reachable = if is_application {
            // Only a genuinely external surface counts: FFI symbols and
            // trait methods reached by dynamic dispatch.
            d.is_ffi || d.trait_impl
        } else {
            d.is_ffi || d.trait_impl || d.is_pub || scan.reexported.contains(d.name())
        };
        if declared || externally_reachable {
            roots.insert(d.key());
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
    let mut roots: std::collections::HashSet<String> =
        std::iter::once(crate::ir::TEST_MODULE_ROOT.to_string()).collect();
    roots.extend(
        scan.defs
            .iter()
            .filter(|d| d.is_test_fn || d.in_test)
            .map(|d| d.key()),
    );
    roots
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
        if ALWAYS_LIVE.contains(&d.name())
            || scan.reexported.contains(d.name())
            || scan.config.is_ignored(d.name())
        {
            continue;
        }
        // Name-based edges cannot distinguish two functions sharing a name,
        // so say nothing when the name is not unique in production code.
        if scan
            .defs
            .iter()
            .filter(|o| o.key() == d.key() && !o.in_test)
            .count()
            > 1
        {
            continue;
        }
        if !prod.contains(&d.key()) && test.contains(&d.key()) {
            let c = scan.calls.get(&d.key());
            let prod_calls = c.map(|c| c.prod).unwrap_or(0);
            out.push(Finding {
                name: d.key(),
                file: d.file.display().to_string(),
                line: d.line,
                test_calls: c.map(|c| c.test).unwrap_or(0),
                examples: c.map(|c| c.examples.clone()).unwrap_or_default(),
                confidence: if prod_calls == 0 {
                    Confidence::High
                } else {
                    Confidence::Medium
                },
                prod_calls,
            });
        }
    }
    out.sort_by(|a, b| b.test_calls.cmp(&a.test_calls).then(a.name.cmp(&b.name)));
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
        let dead = !prod.contains(&d.key());
        s.push_str(&format!(
            "  \"{}\" [style=filled,fillcolor=\"{}\"];\n",
            d.name(),
            if dead { "#ffd6d6" } else { "#e8f0e8" }
        ));
    }
    for (from, tos) in &scan.edges {
        if from.is_empty() {
            continue;
        }
        for to in tos {
            if scan.defs.iter().any(|d| d.name() == to && !d.in_test) {
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
    let mut counts: M<String, usize> = M::new();
    for d in &scan.defs {
        if !d.in_test {
            *counts.entry(d.key()).or_default() += 1;
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
            for nb in fwd
                .get(n)
                .into_iter()
                .flatten()
                .chain(rev.get(n).into_iter().flatten())
            {
                if seen.insert(nb) {
                    queue.push(nb);
                }
            }
        }

        // The frontier is the member with no caller inside the region. If
        // several qualify (or none, in a cycle), prefer the one the tests
        // reach most directly — that is the way in.
        // max_by_key returns the last maximum it sees, and `component` is
        // discovered through hash-ordered adjacency, so ties would resolve
        // differently between runs. Break them on name: the output of a tool
        // that gates CI has to be reproducible, or a baseline diff reports
        // churn that is not in the code.
        fn rank<'x>(by: &M<&str, &Finding>, n: &'x str) -> (usize, std::cmp::Reverse<&'x str>) {
            (by[n].test_calls, std::cmp::Reverse(n))
        }
        let entry_name = component
            .iter()
            .copied()
            .filter(|n| rev.get(n).map(|v| v.is_empty()).unwrap_or(true))
            .max_by_key(|n| rank(&by_name, n))
            .or_else(|| component.iter().copied().max_by_key(|n| rank(&by_name, n)))
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

    regions.sort_by(|a, b| b.size.cmp(&a.size).then(a.entry.name.cmp(&b.entry.name)));
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

    // A definition is keyed by its symbol id, and once a precise tier has
    // promoted it that key is qualified — `uci::parse_uci_line`, not
    // `parse_uci_line`. The graph, the root set and the call counts are all
    // keyed that way, so looking any of them up by the bare name silently
    // finds nothing and reports a live function as reachable by no one.
    //
    // This is the surface the documentation tells people to verify findings
    // with, so being wrong here is worse than being wrong in the report: it
    // makes the tool disagree with itself and gives no way to tell which half
    // is lying.
    //
    // Accept either spelling from the user, and answer over every key the
    // name resolves to.
    let defs: Vec<&FnDef> = scan
        .defs
        .iter()
        .filter(|d| d.name() == name || d.key() == name)
        .collect();
    let d0 = defs.first();

    let mut keys: std::collections::HashSet<String> = defs.iter().map(|d| d.key()).collect();
    // A name with no definition here is still worth asking about: it may be
    // called from this crate and defined elsewhere.
    keys.insert(name.to_string());

    // Every function whose edge list reaches any of those keys.
    let mut callers: Vec<(String, bool)> = scan
        .edges
        .iter()
        .filter(|(_, tos)| tos.iter().any(|t| keys.contains(t)))
        .map(|(from, _)| {
            let label = if from.is_empty() {
                "<module level>".to_string()
            } else {
                from.clone()
            };
            (label, prod.contains(from))
        })
        .collect();
    callers.sort();
    callers.dedup();

    let root_reason = match d0 {
        Some(d) if d.is_ffi => "#[no_mangle] / extern",
        Some(d) if d.trait_impl => "trait impl method (dynamic dispatch)",
        Some(_) if scan.reexported.contains(name) => "re-exported at crate root",
        Some(d) if d.is_pub && scan.entry_files.contains(&d.file) => {
            "exported from the entry module a host loads"
        }
        Some(d) if d.is_pub && !is_application(scan) => "public API of a library crate",
        _ if name == "main" => "program entry point",
        _ => "not a root",
    };

    let ambiguous = scan
        .defs
        .iter()
        .filter(|o| o.name() == name && !o.in_test)
        .count()
        > 1;
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
        defined: defs
            .iter()
            .map(|d| (d.file.display().to_string(), d.line))
            .collect(),
        in_production_set: keys.iter().any(|k| prod.contains(k)),
        in_test_set: keys.iter().any(|k| test.contains(k)),
        is_root: keys.iter().any(|k| roots.contains(k)),
        root_reason,
        callers,
        prod_call_sites: keys
            .iter()
            .filter_map(|k| scan.calls.get(k))
            .map(|c| c.prod)
            .sum(),
        test_call_sites: keys
            .iter()
            .filter_map(|k| scan.calls.get(k))
            .map(|c| c.test)
            .sum(),
        suppressed,
    }
}
