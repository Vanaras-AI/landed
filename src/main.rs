//! landed — find code that shipped but never runs.
//!
//! A feature is written, tests are written for it, and it ships. The tests
//! pass because whoever wrote the code also chose the fixtures. Nothing in
//! production ever calls it, and CI stays green over an absent feature.
//!
//! `landed` builds the crate's call graph and reports functions the tests can
//! reach but the running program cannot.

use landed::{baseline, report, scan};

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "landed",
    version,
    about = "Find code that shipped but never runs"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Record the current findings as accepted, so later runs report only
    /// what is new. Write this file into the repo.
    Baseline {
        /// Path to the crate root (defaults to current directory).
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Baseline the whole-graph analysis rather than the per-function one.
        #[arg(long)]
        graph: bool,

        /// Where to write it.
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
    },

    /// Which tests can reach a change, and which therefore need not run.
    ///
    /// Answered over the permissive graph: omitting a test that would have
    /// caught the change is the expensive mistake, and running one extra is
    /// not. Verify with `scripts/soundness.py` before trusting it to skip
    /// anything.
    Impact {
        /// Path to the project (defaults to current directory).
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Changed functions, by name. Repeatable.
        #[arg(long, value_name = "FN")]
        symbol: Vec<String>,

        /// Take the change from `git diff <REV>` instead.
        #[arg(long, value_name = "REV")]
        since: Option<String>,

        /// Emit JSON.
        #[arg(long)]
        json: bool,

        /// Analyse the tree as this language instead of the detected one.
        #[arg(long, value_name = "LANG")]
        lang: Option<landed::lang::Language>,
    },

    /// Scan a crate for functions that only tests ever call.
    Check {
        /// Path to the crate root (defaults to current directory).
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Emit JSON instead of a human-readable report.
        #[arg(long)]
        json: bool,

        /// Exit with status 1 if any finding exceeds this count (0 = never fail).
        #[arg(long, default_value_t = 0)]
        fail_over: usize,

        /// Resolve calls with the compiler instead of by name. Needs a
        /// nightly toolchain and a crate that compiles; fails rather than
        /// falling back, since the point of the mode is precision.
        #[arg(long)]
        precise: bool,

        /// Use whole-graph reachability instead of the per-function check:
        /// finds entire dead subsystems, not just their outermost function.
        #[arg(long)]
        graph: bool,

        /// Report analysis coverage: how much of the crate the name-based
        /// graph cannot see, and so stays silent about.
        #[arg(long)]
        stats: bool,

        /// List every unreachable function instead of grouping them into
        /// regions. Regions are the default for --graph.
        #[arg(long)]
        flat: bool,

        /// Emit the call graph as Graphviz DOT, unreachable nodes in red.
        #[arg(long)]
        dot: bool,

        /// Output format: text (default), json, github (workflow commands
        /// that annotate the source line), or sarif (code scanning).
        #[arg(long, value_name = "FMT", default_value = "text")]
        format: String,

        /// Compare against a baseline and report only findings that are not
        /// in it. Defaults to .landed-baseline.json beside Cargo.toml.
        #[arg(long, value_name = "FILE", num_args = 0..=1, default_missing_value = "")]
        baseline: Option<String>,

        /// Show every definition and call site recorded for one function name.
        /// Use this to check a finding, or to see why one was not reported.
        #[arg(long, value_name = "FN")]
        explain: Option<String>,

        /// Analyse the tree as this language instead of the detected one.
        /// Detection reads manifests first and file counts second, which is
        /// right for a project but wrong for a polyglot repository where the
        /// part you care about is not the part with the most files.
        #[arg(long, value_name = "LANG")]
        lang: Option<landed::lang::Language>,
    },
}

/// Collect the current findings as baseline entries.
fn entries_now(scan: &scan::Scan, graph: bool, root: &std::path::Path) -> Vec<baseline::Entry> {
    let findings = if graph {
        scan::never_run_graph(scan)
    } else {
        scan::never_run(scan)
    };
    let mut v: Vec<baseline::Entry> = findings
        .into_iter()
        .map(|f| baseline::Entry {
            name: f.name,
            file: baseline::relative(&f.file, root),
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Changed line numbers per file, from `git diff`.
///
/// Returns the hunks it understood and a list of what it could not, because a
/// change this cannot read must widen the answer rather than shrink it.
#[allow(clippy::type_complexity)]
fn changed_lines(
    path: &std::path::Path,
    rev: &str,
) -> anyhow::Result<(Vec<(PathBuf, Vec<usize>)>, Vec<String>)> {
    let out = std::process::Command::new("git")
        .args(["diff", "--unified=0", "--no-color", rev, "--"])
        .current_dir(path)
        .output()
        .map_err(|e| anyhow::anyhow!("could not run git: {e}"))?;
    if !out.status.success() {
        anyhow::bail!(
            "git diff {rev} failed: {}",
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .next()
                .unwrap_or("")
        );
    }
    let text = String::from_utf8_lossy(&out.stdout);

    let mut files: Vec<(PathBuf, Vec<usize>)> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut current: Option<PathBuf> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("+++ b/") {
            current = Some(PathBuf::from(rest.trim()));
            continue;
        }
        if line.starts_with("+++ /dev/null") {
            // A deleted file: nothing left to attribute a line to.
            if let Some(f) = current.take() {
                skipped.push(format!("{} (deleted)", f.display()));
            }
            continue;
        }
        let Some(rest) = line.strip_prefix("@@ ") else {
            continue;
        };
        let Some(plus) = rest.split('+').nth(1) else {
            continue;
        };
        let spec = plus.split(' ').next().unwrap_or("");
        let mut it = spec.split(',');
        let start: usize = match it.next().and_then(|v| v.parse().ok()) {
            Some(v) => v,
            None => continue,
        };
        let count: usize = it.next().and_then(|v| v.parse().ok()).unwrap_or(1);
        if let Some(f) = &current {
            let entry = match files.iter_mut().find(|(p, _)| p == f) {
                Some(e) => e,
                None => {
                    files.push((f.clone(), Vec::new()));
                    files.last_mut().unwrap()
                }
            };
            // A zero-length hunk is a pure deletion; the line that replaced it
            // is the one before.
            let n = count.max(1);
            for i in 0..n {
                entry.1.push(start + i);
            }
        }
    }
    Ok((files, skipped))
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Baseline { path, graph, out } => {
            let scan = scan::scan_crate(&path)?;
            let entries = entries_now(&scan, graph, &path);
            let mode = if graph {
                baseline::Mode::Graph
            } else {
                baseline::Mode::Direct
            };
            let fp = baseline::Fingerprint::of(&scan.config);
            let b = baseline::Baseline::with_fingerprint(mode, entries, Some(fp));
            let file = out.unwrap_or_else(|| baseline::default_path(&path));
            b.save(&file)?;
            println!(
                "wrote {} — {} finding(s) accepted",
                file.display(),
                b.accepted.len()
            );
            println!();
            println!("Commit this file. Later runs with --baseline report only what is");
            println!("new, so CI can gate on additions without demanding the backlog");
            println!("be cleared first.");
        }

        Cmd::Impact {
            path,
            symbol,
            since,
            json,
            lang,
        } => {
            let scan = scan::scan_crate_as(&path, landed::frontend::Tier::Default, lang)?;
            let tests = scan::test_functions(&scan);
            let all_tests = tests.len();
            let opaque = tests.iter().filter(|t| t.opaque).count();

            let mut changed: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut unmapped: Vec<String> = Vec::new();

            for name in &symbol {
                let keys: Vec<String> = scan
                    .defs
                    .iter()
                    .filter(|d| d.name() == name || d.key() == *name)
                    .map(|d| d.key())
                    .collect();
                if keys.is_empty() {
                    unmapped.push(name.clone());
                }
                changed.extend(keys);
            }

            if let Some(rev) = &since {
                let (hunks, skipped) = changed_lines(&path, rev)?;
                for (file, lines) in hunks {
                    let keys = scan::defs_at(&scan, &file, &lines);
                    if keys.is_empty() {
                        unmapped.push(format!("{} (no function matched)", file.display()));
                    }
                    changed.extend(keys);
                }
                unmapped.extend(skipped);
            }

            let hits = scan::tests_reaching(&scan, &changed);

            let mut changed_sorted: Vec<&String> = changed.iter().collect();
            changed_sorted.sort();

            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "schema": 1,
                        "changed_symbols": changed_sorted,
                        "tests_total": all_tests,
                        "tests_always_run": opaque,
                        "tests_affected": hits.len(),
                        "tests": hits,
                        "unmapped": unmapped,
                        "sound": unmapped.is_empty(),
                    })
                );
                return Ok(());
            }

            println!("landed v{}", env!("CARGO_PKG_VERSION"));
            println!("  {all_tests} test(s) found in this project");
            if opaque > 0 {
                println!("  {opaque} of them cross a process boundary and always run");
            }
            println!();

            if changed.is_empty() {
                println!("  No changed function was identified.");
            } else {
                let c = &changed_sorted;
                println!("changed");
                for k in c.iter().take(12) {
                    println!("  {k}");
                }
                if c.len() > 12 {
                    println!("  … and {} more", c.len() - 12);
                }
                println!();
                println!(
                    "affected   {} of {} test(s)",
                    hits.len(),
                    all_tests.max(hits.len())
                );
                for t in hits.iter().take(20) {
                    println!("  {t}");
                }
                if hits.len() > 20 {
                    println!("  … and {} more", hits.len() - 20);
                }
                println!();
                if all_tests > 0 {
                    println!(
                        "reduction  {:.1}%  ({} test(s) need not run)",
                        100.0 * (all_tests.saturating_sub(hits.len())) as f64 / all_tests as f64,
                        all_tests.saturating_sub(hits.len())
                    );
                }
            }

            if !unmapped.is_empty() {
                println!();
                println!(
                    "NOT SOUND — {} part(s) of this change could not be",
                    unmapped.len()
                );
                println!("attributed to a function, so the set below is incomplete:");
                for u in unmapped.iter().take(10) {
                    println!("  {u}");
                }
                println!();
                println!("Run the whole suite. A test-selection answer is only worth");
                println!("having when nothing about the change is unaccounted for.");
                std::process::exit(2);
            }
        }

        Cmd::Check {
            path,
            json,
            fail_over,
            explain,
            graph,
            dot,
            stats,
            flat,
            baseline: baseline_arg,
            format,
            precise,
            lang,
        } => {
            let tier = if precise {
                landed::frontend::Tier::Precise
            } else {
                landed::frontend::Tier::Default
            };
            let scan = scan::scan_crate_as(&path, tier, lang)?;
            let format = if json { "json".to_string() } else { format };

            if let Some(name) = explain {
                let e = scan::evidence(&scan, &name);
                println!("{}\n", e.name);
                if e.defined.is_empty() {
                    println!("  not defined in the scanned crates");
                } else {
                    for (f, l) in &e.defined {
                        println!("  defined      {}:{}", rel(f, &path), l);
                    }
                }
                println!();
                let status = if e.suppressed.is_some() {
                    "NOT ANALYSED"
                } else if e.in_production_set {
                    "reachable from production"
                } else if e.in_test_set {
                    "UNREACHABLE — tests reach it, production cannot"
                } else {
                    "UNREACHABLE — nothing reaches it at all"
                };
                println!("  status       {status}");
                if let Some(why) = e.suppressed {
                    println!("  suppressed   {why}");
                }
                println!(
                    "  entry point  {}",
                    if e.is_root { e.root_reason } else { "no" }
                );
                println!(
                    "  call sites   {} production, {} test",
                    e.prod_call_sites, e.test_call_sites
                );
                println!();
                if e.callers.is_empty() {
                    println!("  callers      none recorded");
                } else {
                    println!(
                        "  callers      ({} recorded — 'live' means the caller is itself",
                        e.callers.len()
                    );
                    println!("               reachable from a production entry point)");
                    for (c, live) in e.callers.iter().take(12) {
                        println!(
                            "                 {:<34} {}",
                            c,
                            if *live { "live" } else { "dead" }
                        );
                    }
                    if e.callers.len() > 12 {
                        println!("                 ... and {} more", e.callers.len() - 12);
                    }
                    let live = e.callers.iter().filter(|(_, l)| *l).count();
                    println!();
                    if live == 0 && !e.callers.is_empty() {
                        println!("  conclusion   every caller is itself unreachable, so this is");
                        println!(
                            "               downstream of a dead region rather than its cause"
                        );
                    } else if live > 0 && !e.in_production_set {
                        println!(
                            "  conclusion   {live} caller(s) are live but no edge to this function"
                        );
                        println!(
                            "               was resolved — likely a limit of name-based matching"
                        );
                    }
                }
                return Ok(());
            }

            if stats {
                let (ambiguous, total) = scan::ambiguity_report(&scan);
                println!("production functions      {total}");
                println!(
                    "non-unique names          {ambiguous} ({:.1}%)",
                    ambiguous as f64 * 100.0 / total.max(1) as f64
                );
                println!();
                println!("Findings are suppressed for non-unique names, because a");
                println!("name-keyed graph cannot tell A::process from B::process.");
                println!("That share of the crate is never reported on, either way.");
                return Ok(());
            }

            if dot {
                print!("{}", scan::to_dot(&scan));
                return Ok(());
            }

            if let Some(arg) = baseline_arg {
                let file = if arg.is_empty() {
                    baseline::default_path(&path)
                } else {
                    PathBuf::from(arg)
                };
                let base = baseline::Baseline::load(&file)?;
                let want = if graph {
                    baseline::Mode::Graph
                } else {
                    baseline::Mode::Direct
                };
                if base.mode != want {
                    anyhow::bail!(
                        "baseline {} was taken in {:?} mode; re-run with the matching \
                         analysis or retake it, because the difference between the two \
                         analyses would be reported as a change in the code",
                        file.display(),
                        base.mode
                    );
                }
                let now = entries_now(&scan, graph, &path);
                let cmp = baseline::compare(&base, &now);

                header(&scan);
                if let Some(why) = base.staleness(&baseline::Fingerprint::of(&scan.config)) {
                    println!("STALE BASELINE — {why}");
                    println!("  Findings can move because the analysis changed, not because the");
                    println!("  code did. Re-take it with `landed baseline` once you have read");
                    println!("  what follows.");
                    println!();
                }
                if !cmp.cleared.is_empty() {
                    println!(
                        "CLEARED — {} baseline finding(s) no longer present",
                        cmp.cleared.len()
                    );
                    for e in cmp.cleared.iter().take(10) {
                        println!("  {}  {}", e.name, e.file);
                    }
                    if cmp.cleared.len() > 10 {
                        println!("  ... and {} more", cmp.cleared.len() - 10);
                    }
                    println!();
                }
                if cmp.added.is_empty() {
                    println!(
                        "No new unreachable code. {} finding(s) carried from the baseline.",
                        cmp.carried
                    );
                } else {
                    println!("NEW — {} finding(s) not in the baseline", cmp.added.len());
                    println!("{}", "─".repeat(74));
                    for e in &cmp.added {
                        println!("  {:32} {}", e.name, e.file);
                    }
                    println!("{}", "─".repeat(74));
                    println!("  {} carried from the baseline, not shown", cmp.carried);
                }
                if !cmp.added.is_empty() && fail_over == 0 {
                    std::process::exit(1);
                }
                if fail_over > 0 && cmp.added.len() > fail_over {
                    std::process::exit(1);
                }
                return Ok(());
            }

            // Regions are the useful unit for whole-graph analysis: a dead
            // subsystem is one work item, not forty findings.
            if graph && !flat {
                let regions = scan::dead_regions(&scan);
                let total: usize = regions.iter().map(|r| r.size).sum();
                match format.as_str() {
                    "json" => println!(
                        "{}",
                        serde_json::to_string_pretty(&report::grouped(&scan, &regions))?
                    ),
                    "github" => print!("{}", report::github_regions(&regions, &path)),
                    "sarif" => println!(
                        "{}",
                        serde_json::to_string_pretty(&report::sarif(&[], Some(&regions), &path))?
                    ),
                    _ => report_regions(&scan, &regions, total, &path),
                }
                if fail_over > 0 && total > fail_over {
                    std::process::exit(1);
                }
                return Ok(());
            }

            let findings = if graph {
                scan::never_run_graph(&scan)
            } else {
                scan::never_run(&scan)
            };

            match format.as_str() {
                "json" => println!(
                    "{}",
                    serde_json::to_string_pretty(&report::flat(&scan, &findings, graph))?
                ),
                "github" => print!("{}", report::github(&findings, &path)),
                "sarif" => println!(
                    "{}",
                    serde_json::to_string_pretty(&report::sarif(&findings, None, &path))?
                ),
                _ => report(&scan, &findings, &path),
            }

            if fail_over > 0 && findings.len() > fail_over {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

fn report(scan: &scan::Scan, findings: &[scan::Finding], root: &std::path::Path) {
    let prod_defs = scan.defs.iter().filter(|d| !d.in_test).count();

    println!("landed v{}", env!("CARGO_PKG_VERSION"));
    println!("  {prod_defs} production functions scanned\n");

    if scan.defs.is_empty() {
        nothing_read(scan);
        return;
    }

    if findings.is_empty() {
        println!("  No never-run functions found.");
        return;
    }

    precision_caveat(scan, findings.len());
    println!("NEVER RUN — defined in production, called only by tests");
    println!("{}", "─".repeat(78));

    for f in findings {
        println!("\n  {}", f.name);
        println!("    defined  {}:{}", rel(&f.file, root), f.line);
        // This said "0 production calls" outright, which is true of the
        // direct check and false of `--graph --flat`, where a finding may
        // have production callers that are themselves unreachable. Printing
        // it as a certainty made an uncertain finding read as a confident
        // one — the one kind of error this report must not make.
        if f.prod_calls == 0 {
            println!(
                "    callers  {} test call(s), no production caller",
                f.test_calls
            );
        } else {
            println!(
                "    callers  {} test call(s), {} production call(s) — but every",
                f.test_calls, f.prod_calls
            );
            println!("             caller is itself unreachable, so this is uncertain");
        }
        for e in f.examples.iter().take(2) {
            println!("             {}", rel(e, root));
        }
    }

    println!("\n{}", "─".repeat(78));
    println!("  {} function(s) shipped that nothing runs", findings.len());
}

/// Header shared by every report: what was scanned, and what could not be.
/// Precise mode should mostly *remove* findings, by settling names the
/// nominal tier declined to judge. When it adds many instead, the likelier
/// explanation is a call form its parser did not read than a sudden abundance
/// of dead code — so the report says so rather than presenting the number as
/// fact.
fn precision_caveat(scan: &scan::Scan, found: usize) {
    let Some(nominal) = scan.nominal_findings else {
        return;
    };
    if found <= nominal.saturating_add(nominal / 5).max(nominal + 2) {
        return;
    }
    println!("CAUTION — precise mode reports {found} findings; the default reports {nominal}.");
    println!("  This tier resolves identity from the compiler's human-readable MIR");
    println!("  dump. A call form its parser does not recognise is a call it does not");
    println!("  see, and everything that call reached then looks unreachable. A large");
    println!("  increase is more likely that than newly discovered dead code.");
    println!("  Treat the additions as leads, verify with --explain, and trust the");
    println!("  default tier where the two disagree.");
    println!();
}

/// A scan that read nothing has nothing to say, and must not say it in the
/// words of a clean bill of health. The likeliest causes are a wrong language
/// and a path with no source under it, so it names what it looked for.
fn nothing_read(scan: &scan::Scan) {
    println!("  No source was read.");
    println!();
    println!("  The project was read as: {}", scan.project_description);
    println!();
    println!("  This is not a clean result — nothing was analysed. Either the");
    println!("  path holds no source of that language, or detection picked the");
    println!("  wrong one. State it with --lang <rust|python|typescript|go>.");
}

fn header(scan: &scan::Scan) {
    let prod = scan.defs.iter().filter(|d| !d.in_test).count();
    let (ambiguous, total) = scan::ambiguity_report(scan);
    println!("landed v{}", env!("CARGO_PKG_VERSION"));
    println!("  {prod} production functions scanned");
    // The single most consequential decision the tool makes, and it used to
    // be invisible: a library's whole public surface is a root, so calling an
    // application a library reports nothing, and the reverse reports
    // everything. Anyone doubting a result should see this first.
    if !scan.project_description.is_empty() {
        println!("  read as {}", scan.project_description);
    }
    if ambiguous > 0 {
        println!(
            "  {ambiguous} ({:.1}%) share a name with another function and are not analysed",
            ambiguous as f64 * 100.0 / total.max(1) as f64
        );
    }
    println!();
}

/// Print a location relative to the crate that was scanned. Absolute paths
/// are unreadable in a terminal and unusable in CI annotations.
fn rel(file: &str, root: &std::path::Path) -> String {
    let r = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let rs = r.to_string_lossy().to_string();
    file.strip_prefix(&rs)
        .map(|s| s.trim_start_matches('/').to_string())
        .unwrap_or_else(|| {
            // fall back to the path from the crate's src/ directory
            file.rsplit_once("/src/")
                .map(|(_, tail)| format!("src/{tail}"))
                .unwrap_or_else(|| file.to_string())
        })
}

fn report_regions(
    scan: &scan::Scan,
    regions: &[scan::Region],
    total: usize,
    root: &std::path::Path,
) {
    header(scan);
    precision_caveat(scan, total);

    if scan.defs.is_empty() {
        nothing_read(scan);
        return;
    }

    if regions.is_empty() {
        println!("  Everything is reachable from production entry points.");
        return;
    }

    let (multi, single): (Vec<_>, Vec<_>) = regions.iter().partition(|r| r.size > 1);

    println!(
        "{total} unreachable function{} in {} region{}",
        if total == 1 { "" } else { "s" },
        regions.len(),
        if regions.len() == 1 { "" } else { "s" }
    );
    let hi = regions
        .iter()
        .filter(|r| r.confidence == scan::Confidence::High)
        .count();
    println!(
        "  {hi} confident, {} uncertain (production callers exist but look unreachable)",
        regions.len() - hi
    );
    if !single.is_empty() {
        println!(
            "  {} subsystem{} of 2+ functions, and {} lone function{}",
            multi.len(),
            if multi.len() == 1 { "" } else { "s" },
            single.len(),
            if single.len() == 1 { "" } else { "s" }
        );
    }

    for (i, r) in multi.iter().enumerate() {
        println!("\n{}", "─".repeat(74));
        let conf = match r.confidence {
            scan::Confidence::High => "confident",
            scan::Confidence::Medium => {
                "UNCERTAIN — may be an entry point this analyzer cannot see"
            }
        };
        println!(
            "Region {} — {} function{}  [{}]",
            i + 1,
            r.size,
            if r.size == 1 { "" } else { "s" },
            conf
        );
        println!("{}", "─".repeat(74));
        println!("  frontier   {}", r.entry.name);
        println!("             {}:{}", rel(&r.entry.file, root), r.entry.line);
        println!();
        if r.entry.prod_calls > 0 {
            println!(
                "  entered    {} production call(s) and {} test call(s) — but every",
                r.entry.prod_calls, r.entry.test_calls
            );
            println!("             caller is itself unreachable, so this is either a dead");
            println!("             subsystem or a root the analyzer failed to resolve");
        } else if r.entry.test_calls > 0 {
            println!(
                "  entered    {} test call(s), 0 from production",
                r.entry.test_calls
            );
            for e in r.entry.examples.iter().take(2) {
                println!("             {}", rel(e, root));
            }
        } else {
            println!("  entered    nothing calls it, in tests or production");
        }
        if r.files.len() > 1 {
            println!("\n  spans      {} files", r.files.len());
        }
        if !r.members.is_empty() {
            println!(
                "\n  downstream {} function(s) reachable only through the frontier:",
                r.members.len()
            );
            let show: Vec<&str> = r.members.iter().take(6).map(|m| m.name.as_str()).collect();
            println!("             {}", show.join(", "));
            if r.members.len() > show.len() {
                println!("             ... and {} more", r.members.len() - show.len());
            }
        }
        println!(
            "\n  fix        make {} reachable, or delete the region",
            r.entry.name
        );
    }

    if !single.is_empty() {
        println!("\n{}", "─".repeat(74));
        println!(
            "Lone functions — {} with no unreachable callees",
            single.len()
        );
        println!("{}", "─".repeat(74));
        for r in single.iter().take(20) {
            println!(
                "  {:32} {}:{}",
                r.entry.name,
                rel(&r.entry.file, root),
                r.entry.line
            );
        }
        if single.len() > 20 {
            println!(
                "  ... and {} more (--json for the full list)",
                single.len() - 20
            );
        }
    }

    println!("\n{}", "─".repeat(74));
    println!("  Start at each subsystem's frontier. Everything downstream of it");
    println!("  resolves when the frontier does.");
}
