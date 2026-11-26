//! Executable plugins; the wire contract lives in `docs/plugins.md` and the
//! `conary-plugin` crate.
mod runner;
pub use conary_plugin::*;
pub use runner::{Limits, Plugin};
