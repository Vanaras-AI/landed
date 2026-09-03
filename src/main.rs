//! landed — find code that shipped but never runs.
//!
//! A feature is written, tests are written for it, and it ships. The tests
//! pass because whoever wrote the code also chose the fixtures. Nothing in
//! production ever calls it, and CI stays green over an absent feature.
//!
//! `landed` builds the crate's call graph and reports functions the tests can
//! reach but the running program cannot.

mod scan;

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

        /// Use whole-graph reachability instead of the per-function check:
        /// finds entire dead subsystems, not just their outermost function.
        #[arg(long)]
        graph: bool,

        /// Report analysis coverage: how much of the crate the name-based
        /// graph cannot see, and so stays silent about.
        #[arg(long)]
        stats: bool,

        /// Emit the call graph as Graphviz DOT, unreachable nodes in red.
        #[arg(long)]
        dot: bool,

        /// Show every definition and call site recorded for one function name.
        /// Use this to check a finding, or to see why one was not reported.
        #[arg(long, value_name = "FN")]
        explain: Option<String>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Check {
            path,
            json,
            fail_over,
            explain,
            graph,
            dot,
            stats,
        } => {
            let scan = scan::scan_crate(&path)?;

            if let Some(name) = explain {
                println!("crates scanned:");
                for r in scan::resolve_roots(&path) {
                    println!("  {}", r.display());
                }
                println!("\ndefinitions of `{name}`:");
                for d in scan.defs.iter().filter(|d| d.name == name) {
                    println!(
                        "  {}:{}   test={} trait_impl={} allow_dead={}",
                        d.file.display(),
                        d.line,
                        d.in_test,
                        d.trait_impl,
                        d.allowed_dead
                    );
                }
                match scan.calls.get(&name) {
                    Some(c) => {
                        println!(
                            "\ncall sites: {} production, {} test",
                            c.prod, c.test
                        );
                        for e in &c.examples {
                            println!("  test: {e}");
                        }
                    }
                    None => println!("\ncall sites: none recorded"),
                }
                println!(
                    "\nre-exported at crate root: {}",
                    scan.reexported.contains(&name)
                );
                return Ok(());
            }

            if stats {
                let (a, t) = scan::ambiguity_report(&scan);
                println!("production functions      {t}");
                println!("non-unique names          {a} ({:.1}%)", a as f64 * 100.0 / t.max(1) as f64);
                println!();
                println!("Findings are suppressed for non-unique names, because a");
                println!("name-keyed graph cannot tell A::process from B::process.");
                println!("That fraction of the crate is therefore never reported on.");
                return Ok(());
            }

            if dot {
                print!("{}", scan::to_dot(&scan));
                return Ok(());
            }

            let findings = if graph {
                scan::never_run_graph(&scan)
            } else {
                scan::never_run(&scan)
            };

            if json {
                println!("{}", serde_json::to_string_pretty(&findings)?);
            } else {
                report(&scan, &findings);
            }

            if fail_over > 0 && findings.len() > fail_over {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

fn report(scan: &scan::Scan, findings: &[scan::Finding]) {
    let prod_defs = scan.defs.iter().filter(|d| !d.in_test).count();

    println!("landed v{}", env!("CARGO_PKG_VERSION"));
    println!("  {prod_defs} production functions scanned\n");

    if findings.is_empty() {
        println!("  No never-run functions found.");
        return;
    }

    println!("NEVER RUN — defined in production, called only by tests");
    println!("{}", "─".repeat(78));

    for f in findings {
        let loc = format!("{}:{}", f.file, f.line);
        println!("\n  {}", f.name);
        println!("    defined  {loc}");
        println!(
            "    callers  {} test call(s), 0 production calls",
            f.test_calls
        );
        for e in f.examples.iter().take(2) {
            println!("             {e}");
        }
    }

    println!("\n{}", "─".repeat(78));
    println!("  {} function(s) shipped that nothing runs", findings.len());
}
