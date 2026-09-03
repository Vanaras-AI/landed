//! Machine-readable output.
//!
//! JSON here is an API, not a dump of whatever the CLI happened to print. It
//! carries a schema version so a consumer can refuse a format it does not
//! understand rather than silently misread it, and a summary so a dashboard
//! does not have to re-derive totals the analyzer already knows.

use crate::scan::{Confidence, Finding, Region, Scan};
use serde::Serialize;

/// Bumped when a field changes meaning or disappears. Additive fields do not
/// bump it; consumers are expected to ignore unknown keys.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize)]
pub struct Summary {
    /// Functions defined outside test code.
    pub production_functions: usize,
    /// Of those, how many share a name with another and are therefore never
    /// judged. A rate computed without this number overstates its coverage.
    pub unanalysable_names: usize,
    pub unreachable: usize,
    pub confident: usize,
    pub uncertain: usize,
    pub regions: usize,
}

#[derive(Debug, Serialize)]
pub struct Report<'a> {
    pub schema: u32,
    pub tool: &'static str,
    pub tool_version: &'static str,
    /// "direct" (per-function) or "graph" (whole-graph reachability).
    pub mode: &'static str,
    pub summary: Summary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub findings: Option<&'a [Finding]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub regions: Option<&'a [Region]>,
}

fn summarize(scan: &Scan, findings: &[Finding], regions: usize) -> Summary {
    let (unanalysable, _) = crate::scan::ambiguity_report(scan);
    Summary {
        production_functions: scan.defs.iter().filter(|d| !d.in_test).count(),
        unanalysable_names: unanalysable,
        unreachable: findings.len(),
        confident: findings.iter().filter(|f| f.confidence == Confidence::High).count(),
        uncertain: findings.iter().filter(|f| f.confidence == Confidence::Medium).count(),
        regions,
    }
}

pub fn flat<'a>(scan: &Scan, findings: &'a [Finding], graph: bool) -> Report<'a> {
    Report {
        schema: SCHEMA_VERSION,
        tool: "landed",
        tool_version: env!("CARGO_PKG_VERSION"),
        mode: if graph { "graph" } else { "direct" },
        summary: summarize(scan, findings, 0),
        findings: Some(findings),
        regions: None,
    }
}

pub fn grouped<'a>(scan: &Scan, regions: &'a [Region]) -> Report<'a> {
    let flat: Vec<Finding> = Vec::new();
    let mut s = summarize(scan, &flat, regions.len());
    s.unreachable = regions.iter().map(|r| r.size).sum();
    s.confident = regions
        .iter()
        .filter(|r| r.confidence == Confidence::High)
        .map(|r| r.size)
        .sum();
    s.uncertain = s.unreachable - s.confident;
    Report {
        schema: SCHEMA_VERSION,
        tool: "landed",
        tool_version: env!("CARGO_PKG_VERSION"),
        mode: "graph",
        summary: s,
        findings: None,
        regions: Some(regions),
    }
}

// ─── CI annotations ───────────────────────────────────────────

/// GitHub Actions workflow commands, which annotate the exact source line in
/// a pull request diff rather than leaving the finding in log output nobody
/// opens.
///
/// Uncertain findings are emitted as `notice` rather than `warning`: a
/// finding the analyzer itself is unsure about should not decorate someone's
/// diff with the same weight as one it can prove.
pub fn github(findings: &[Finding], root: &std::path::Path) -> String {
    let mut out = String::new();
    for f in findings {
        let level = match f.confidence {
            Confidence::High => "warning",
            Confidence::Medium => "notice",
        };
        let file = crate::baseline::relative(&f.file, root);
        let detail = match f.confidence {
            Confidence::High => format!(
                "{} is never called outside tests ({} test call(s), 0 from production)",
                f.name, f.test_calls
            ),
            Confidence::Medium => format!(
                "{} may be unreachable: {} production caller(s) exist but each looks \
                 unreachable itself, which can also mean an entry point this analyzer \
                 cannot resolve",
                f.name, f.prod_calls
            ),
        };
        out.push_str(&format!(
            "::{level} file={file},line={line},title=landed::{detail}\n",
            line = f.line
        ));
    }
    out
}

/// Region-aware annotations: one per frontier, since everything downstream of
/// it resolves when it does. Annotating forty functions for one dead
/// subsystem buries the diff.
pub fn github_regions(regions: &[Region], root: &std::path::Path) -> String {
    let mut out = String::new();
    for r in regions {
        let level = match r.confidence {
            Confidence::High => "warning",
            Confidence::Medium => "notice",
        };
        let file = crate::baseline::relative(&r.entry.file, root);
        let detail = if r.size > 1 {
            format!(
                "{} is the entry to {} functions nothing in production reaches; \
                 they resolve when it does",
                r.entry.name, r.size
            )
        } else {
            format!("{} is never called outside tests", r.entry.name)
        };
        out.push_str(&format!(
            "::{level} file={file},line={line},title=landed::{detail}\n",
            line = r.entry.line
        ));
    }
    out
}
