// crates/conary-core/src/scriptlet/native_lifecycle/preflight_error.rs
//! Typed runtime requirements checked before staging or lifecycle execution.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NativeLifecyclePreflightError {
    #[error("Native lifecycle execution requires an absolute materialized selected root other than '/': {}", root.display())]
    InvalidExecutionRoot { root: PathBuf },
    #[error(
        "Interpreter not found: {interpreter}. Cannot execute native lifecycle entry '{entry_id}'."
    )]
    MissingInterpreter {
        interpreter: String,
        entry_id: String,
        /// The missing path was checked in the current root or its event projection.
        projected: bool,
    },
    #[error(
        "TimeoutOutOfRange: native lifecycle entry '{entry_id}' timeout_ms {timeout_ms} is outside {minimum_ms}..={maximum_ms}"
    )]
    TimeoutOutOfRange {
        entry_id: String,
        timeout_ms: u64,
        minimum_ms: u64,
        maximum_ms: u64,
    },
}
