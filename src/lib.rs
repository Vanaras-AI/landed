//! Analysis engine for `landed`.
//!
//! Deliberately separate from the binary: the CLI is one consumer of this
//! engine, and keeping presentation out of the analysis is what allows the
//! same code to be tested directly, and later to back a CI action or an
//! editor integration.
//!
//! ```no_run
//! let scan = landed::scan::scan_crate(std::path::Path::new("src"))?;
//! for region in landed::scan::dead_regions(&scan) {
//!     println!("{} — {} functions", region.entry.name, region.size);
//! }
//! # Ok::<(), anyhow::Error>(())
//! ```

pub mod baseline;
pub mod config;
pub mod report;
pub mod scan;
