//! Project configuration: `landed.toml` at the crate root.
//!
//! Entry points are the hardest part of reachability, and no heuristic gets
//! them right everywhere. A binary launched through `tokio::spawn`, a handler
//! stored in a registry, a callback held in a struct field — each breaks the
//! chain from `main`, and everything downstream is then reported as dead.
//! That happened on a real codebase: an entire live PII-redaction subsystem
//! was condemned because the connectors that call it are spawned as tasks.
//!
//! Rather than guess harder, let the developer say what production means:
//!
//! ```toml
//! # landed.toml
//! roots  = ["handle_webhook", "daemon_process_*"]
//! ignore = ["generated_*", "legacy_shim"]
//! ```

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Extra production entry points. Anything reachable from these is live.
    /// Patterns may use `*`.
    #[serde(default)]
    pub roots: Vec<String>,

    /// Never report these, whatever the analysis concludes. For generated
    /// code, deliberate shims, and anything already known and accepted.
    #[serde(default)]
    pub ignore: Vec<String>,
}

impl Config {
    /// Load `landed.toml` from a crate root, or from the parent when handed a
    /// `src/` directory. Absent config is not an error — it is the norm.
    pub fn load(scan_path: &Path) -> anyhow::Result<Self> {
        for dir in [scan_path, scan_path.parent().unwrap_or(scan_path)] {
            let f = dir.join("landed.toml");
            if f.is_file() {
                let text = std::fs::read_to_string(&f)?;
                let c: Config =
                    toml::from_str(&text).map_err(|e| anyhow::anyhow!("{}: {e}", f.display()))?;
                return Ok(c);
            }
        }
        Ok(Config::default())
    }

    pub fn is_root(&self, name: &str) -> bool {
        self.roots.iter().any(|p| matches(p, name))
    }

    pub fn is_ignored(&self, name: &str) -> bool {
        self.ignore.iter().any(|p| matches(p, name))
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty() && self.ignore.is_empty()
    }
}

/// Glob match supporting `*` only. Deliberately not a regex: a config file
/// that can express a catastrophic backtracking pattern is a footgun, and
/// `*` covers every case this needs.
pub fn matches(pattern: &str, name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == name;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        match name[pos..].find(part) {
            Some(found) => {
                // A leading literal must anchor at the start.
                if i == 0 && found != 0 {
                    return false;
                }
                pos += found + part.len();
            }
            None => return false,
        }
    }
    // A trailing literal must reach the end.
    match parts.last() {
        Some(last) if !last.is_empty() => name.ends_with(last),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn exact_and_wildcards() {
        assert!(matches("run", "run"));
        assert!(!matches("run", "runner"));
        assert!(matches("*", "anything"));
        assert!(matches("handle_*", "handle_webhook"));
        assert!(!matches("handle_*", "unhandle_webhook"));
        assert!(matches("*_handler", "webhook_handler"));
        assert!(!matches("*_handler", "handler_webhook"));
        assert!(matches("daemon_*_message", "daemon_process_message"));
        assert!(!matches("daemon_*_message", "daemon_process_event"));
    }
}
