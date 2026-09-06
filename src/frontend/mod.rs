//! Frontends: the only place that knows how a fact about the code was
//! obtained.
//!
//! Each produces the same `Extract`, differing in precision and in what it
//! demands of the environment. The analysis layer consumes that and never
//! learns which one ran.

pub mod mir_frontend;
pub mod syn_frontend;
pub mod tree_sitter_frontend;

pub use mir_frontend::MirFrontend;
pub use syn_frontend::SynFrontend;
pub use tree_sitter_frontend::TreeSitterFrontend;

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
    for_tier_in(tier, crate::lang::Language::Rust)
}

/// The frontend for a tier in a given language.
///
/// Only Rust has a precise tier: it is the only language here with a compiler
/// whose resolved output this tool reads. Asking for precision elsewhere is an
/// error rather than a silent downgrade — the same rule the Rust tiers follow.
pub fn for_tier_in(tier: Tier, lang: crate::lang::Language) -> anyhow::Result<Box<dyn Frontend>> {
    use crate::lang::Language;
    match (tier, lang) {
        (Tier::Default, Language::Rust) => Ok(Box::new(SynFrontend)),
        (Tier::Precise, Language::Rust) => Ok(Box::new(MirFrontend)),
        (Tier::Default, other) => Ok(Box::new(TreeSitterFrontend { language: other })),
        (Tier::Precise, other) => Err(anyhow::anyhow!(
            "--precise is only available for Rust; this looks like a {} project.\n\
             \n\
             Precision comes from reading the compiler's resolved output, and \
             only the Rust frontend does that. Run without --precise for the \
             syntactic analysis, which reports what it cannot resolve rather \
             than guessing — see `landed check --stats`.",
            other.name()
        )),
    }
}

/// Run a tier over a path, choosing the frontend by what the project is.
pub fn extract(tier: Tier, root: &Path) -> anyhow::Result<Extract> {
    extract_as(tier, root, None)
}

/// As `extract`, with the language stated rather than detected.
pub fn extract_as(
    tier: Tier,
    root: &Path,
    lang: Option<crate::lang::Language>,
) -> anyhow::Result<Extract> {
    let lang = lang
        .or_else(|| crate::lang::detect(root))
        .unwrap_or(crate::lang::Language::Rust);
    for_tier_in(tier, lang)?.extract(root)
}

/// Best precision a tier can produce, without running it.
pub fn precision(tier: Tier) -> Precision {
    match tier {
        Tier::Default => Precision::Nominal,
        // Typed, not Resolved: the MIR dump names free functions without a
        // module path, so two same-named functions in different modules stay
        // indistinguishable. Methods are raised to Typed, which is where the
        // ambiguity actually is.
        Tier::Precise => Precision::Typed,
    }
}
