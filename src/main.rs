//! landed — find code that shipped but never runs.
//!
//! An AI agent writes a feature, writes its tests, and reports success. The
//! tests pass because the agent also chose the fixtures. Nothing in production
//! ever calls the feature. CI is green and the work is absent.
//!
//! `landed` reads the crate and reports functions whose only callers are tests.

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
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Check {
            path,
            json,
            fail_over,
        } => {
            let scan = scan::scan_crate(&path)?;
            let findings = scan::never_run(&scan);

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
