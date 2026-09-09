// crates/conary-agent-contract/src/lib.rs
//! Transport-neutral operation contract for Conary agent-facing workflows.

pub mod catalog;
pub mod package_failure;
pub mod resource;
pub mod result;
pub mod verification;

pub use catalog::*;
pub use package_failure::*;
pub use resource::*;
pub use result::*;
pub use verification::*;
