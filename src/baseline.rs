//! Baselines: adopt the tool without fixing the backlog first.
//!
//! A codebase that has never been analysed will produce findings on the first
//! run — one had 216. Presented as a wall, that is uninstallable: the honest
//! response is to uninstall the tool, not to fix 216 functions.
//!
//! A baseline records what was already there, so CI can gate on *new*
//! unreachable code while the backlog is dealt with separately, or never. It
//! turns "clean up five years of history" into "stop adding to it".

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Default filename, kept next to `Cargo.toml` and committed to the repo.
pub const DEFAULT_FILE: &str = ".landed-baseline.json";

/// Which analysis produced a baseline.
///
/// Recorded because the two modes answer different questions, and comparing a
/// graph baseline against per-function findings would report the difference
/// between the analyses as if it were a change in the code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Per-function: does any non-test caller exist?
    Direct,
    /// Whole-graph reachability from production entry points.
    Graph,
}

/// One accepted finding.
///
/// Identified by name and file, deliberately not by line: a finding that moved
/// because something above it grew is the same finding, and keying on line
/// would report it as new on the next unrelated edit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    /// Relative to the crate root, so the file is portable between machines.
    pub file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Baseline {
    /// Schema version. Present so a future format change can be detected
    /// rather than silently misread.
    pub version: u32,
    pub created: String,
    pub mode: Mode,
    /// Sorted, so the file has a stable diff in review.
    pub accepted: BTreeSet<Entry>,
}

impl Baseline {
    pub fn new(mode: Mode, entries: impl IntoIterator<Item = Entry>) -> Self {
        Self {
            version: 1,
            created: chrono_now(),
            mode,
            accepted: entries.into_iter().collect(),
        }
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            anyhow::anyhow!("could not read baseline {}: {e}", path.display())
        })?;
        let b: Baseline = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("{} is not a valid baseline: {e}", path.display()))?;
        if b.version != 1 {
            anyhow::bail!(
                "baseline {} is version {}, this build understands version 1",
                path.display(),
                b.version
            );
        }
        Ok(b)
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)? + "\n")?;
        Ok(())
    }
}

/// What changed since the baseline was taken.
#[derive(Debug, Default)]
pub struct Comparison {
    /// Findings not in the baseline. These are what CI should fail on.
    pub added: Vec<Entry>,
    /// Baseline entries that no longer appear — fixed, deleted, or renamed.
    pub cleared: Vec<Entry>,
    /// Still present, still accepted.
    pub carried: usize,
}

pub fn compare(baseline: &Baseline, current: &[Entry]) -> Comparison {
    let now: BTreeSet<&Entry> = current.iter().collect();
    let mut c = Comparison::default();
    for e in current {
        if baseline.accepted.contains(e) {
            c.carried += 1;
        } else {
            c.added.push(e.clone());
        }
    }
    for e in &baseline.accepted {
        if !now.contains(e) {
            c.cleared.push(e.clone());
        }
    }
    c.added.sort();
    c.cleared.sort();
    c
}

/// Path of a finding relative to the crate root, so baselines survive being
/// checked out somewhere else.
pub fn relative(file: &str, root: &Path) -> String {
    let r = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let rs = r.to_string_lossy().to_string();
    file.strip_prefix(&rs)
        .map(|s| s.trim_start_matches('/').to_string())
        .unwrap_or_else(|| {
            file.rsplit_once("/src/")
                .map(|(_, tail)| format!("src/{tail}"))
                .unwrap_or_else(|| file.to_string())
        })
}

/// Where the baseline lives for a given scan path: alongside `Cargo.toml` if
/// there is one, otherwise in the directory itself.
pub fn default_path(scan_path: &Path) -> PathBuf {
    let dir = if scan_path.join("Cargo.toml").is_file() {
        scan_path.to_path_buf()
    } else if scan_path.file_name().map(|n| n == "src").unwrap_or(false) {
        scan_path.parent().unwrap_or(scan_path).to_path_buf()
    } else {
        scan_path.to_path_buf()
    };
    dir.join(DEFAULT_FILE)
}

/// UTC date-time for a human reading the file. Nothing parses it, so this
/// avoids a date dependency for one field.
fn chrono_now() -> String {
    let secs = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(_) => return "unknown".into(),
    };
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };

    format!("{y:04}-{mth:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}
