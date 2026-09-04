//! Frontends: the only place that knows how a fact about the code was
//! obtained.
//!
//! Each produces the same `Extract`, differing in precision and in what it
//! demands of the environment. The analysis layer consumes that and never
//! learns which one ran.

pub mod syn_frontend;

pub use syn_frontend::SynFrontend;

use crate::ir::{Extract, Frontend, Precision};
use std::path::Path;

/// Which analysis tier to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// `syn`. No build, no toolchain requirement, works on code that does not
    /// compile. Nominal precision, so ambiguous names go unjudged.
    Default,
    /// Compiler-resolved. Exact call targets, at the cost of requiring a
    /// successful build.
    Precise,
}

/// Build the frontend for a tier.
///
/// `Precise` returns an error rather than degrading: a mode whose entire
/// purpose is precision must never silently answer with something less.
pub fn for_tier(tier: Tier) -> anyhow::Result<Box<dyn Frontend>> {
    match tier {
        Tier::Default => Ok(Box::new(SynFrontend)),
        Tier::Precise => Err(anyhow::anyhow!(
            "--precise is not implemented in this build.\n\
             \n\
             It requires a compiler-resolved frontend (MIR). Until that lands, \
             run without --precise for the syntactic analysis, which reports \
             what it cannot resolve rather than guessing — see `landed check \
             --stats`."
        )),
    }
}

/// Run a tier over a path.
pub fn extract(tier: Tier, root: &Path) -> anyhow::Result<Extract> {
    for_tier(tier)?.extract(root)
}

/// Best precision a tier can produce, without running it.
pub fn precision(tier: Tier) -> Precision {
    match tier {
        Tier::Default => Precision::Nominal,
        Tier::Precise => Precision::Resolved,
    }
}
