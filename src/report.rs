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

// ─── SARIF ────────────────────────────────────────────────────

/// SARIF 2.1.0, the format GitHub code scanning and most other analysis
/// dashboards ingest. Emitting it is how findings reach the Security tab and
/// survive across runs, rather than living in one job's log.
///
/// Rules are declared once and referenced by id, so a dashboard can group,
/// suppress and track a rule over time instead of treating every finding as
/// unrelated text.
pub fn sarif(
    findings: &[Finding],
    regions: Option<&[Region]>,
    root: &std::path::Path,
) -> serde_json::Value {
    use serde_json::json;

    let rules = json!([
        {
            "id": "unreachable-function",
            "name": "UnreachableFunction",
            "shortDescription": { "text": "Function is never called outside tests" },
            "fullDescription": {
                "text": "This function exists in production code, but no production \
                         caller reaches it. The tests exercise it, so it compiles and \
                         passes CI while never running."
            },
            "defaultConfiguration": { "level": "warning" },
            "properties": { "tags": ["dead-code", "reachability"] }
        },
        {
            "id": "unreachable-region",
            "name": "UnreachableRegion",
            "shortDescription": { "text": "Entry point of a subsystem nothing reaches" },
            "fullDescription": {
                "text": "Everything downstream of this function is reachable only \
                         through it, and nothing in production reaches it. Resolving \
                         this function resolves the whole region."
            },
            "defaultConfiguration": { "level": "warning" },
            "properties": { "tags": ["dead-code", "reachability"] }
        },
        {
            "id": "possibly-unreachable",
            "name": "PossiblyUnreachable",
            "shortDescription": { "text": "Unreachable, but the analyzer is unsure" },
            "fullDescription": {
                "text": "Production callers exist, but each of them also looks \
                         unreachable. That is either a dead subsystem or an entry \
                         point this analyzer cannot resolve — async spawns, trait \
                         objects and stored closures all break name-based edges."
            },
            "defaultConfiguration": { "level": "note" },
            "properties": { "tags": ["dead-code", "reachability", "low-confidence"] }
        }
    ]);

    let mut results = Vec::new();

    let mut push = |rule: &str, level: &str, text: String, file: &str, line: usize| {
        results.push(json!({
            "ruleId": rule,
            "level": level,
            "message": { "text": text },
            "locations": [{
                "physicalLocation": {
                    "artifactLocation": { "uri": file, "uriBaseId": "%SRCROOT%" },
                    "region": { "startLine": line.max(1) }
                }
            }],
            // Stable across line shifts, so a dashboard can track one finding
            // through unrelated edits instead of closing and reopening it.
            "partialFingerprints": { "landed/v1": format!("{file}:{rule}") }
        }));
    };

    match regions {
        Some(rs) => {
            for r in rs {
                let (rule, level) = match r.confidence {
                    Confidence::High => ("unreachable-region", "warning"),
                    Confidence::Medium => ("possibly-unreachable", "note"),
                };
                let text = if r.size > 1 {
                    format!(
                        "{} is the entry to {} functions nothing in production reaches; \
                         they resolve when it does",
                        r.entry.name, r.size
                    )
                } else {
                    format!("{} is never called outside tests", r.entry.name)
                };
                push(
                    rule,
                    level,
                    text,
                    &crate::baseline::relative(&r.entry.file, root),
                    r.entry.line,
                );
            }
        }
        None => {
            for f in findings {
                let (rule, level) = match f.confidence {
                    Confidence::High => ("unreachable-function", "warning"),
                    Confidence::Medium => ("possibly-unreachable", "note"),
                };
                let text = format!(
                    "{} is never called outside tests ({} test call(s), {} from production)",
                    f.name, f.test_calls, f.prod_calls
                );
                push(rule, level, text, &crate::baseline::relative(&f.file, root), f.line);
            }
        }
    }

    json!({
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": { "driver": {
                "name": "landed",
                "version": env!("CARGO_PKG_VERSION"),
                "informationUri": "https://github.com/Vanaras-AI/landed",
                "rules": rules
            }},
            "results": results
        }]
    })
}
