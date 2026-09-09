// apps/conary/src/commands/install/native_events/refusal.rs
//! Context attached only at the transaction-wide preflight boundary.

use conary_core::ccs::native_transaction::{
    NativeEventProgram, NativeEventStage, NativeTransactionEvent,
};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
#[error("native transaction preflight failed for {package} {version}")]
pub(crate) struct NativePreflightContext {
    pub package: String,
    pub version: String,
    pub architecture: Option<String>,
    pub source_format: String,
    pub root: PathBuf,
    pub stage: NativeEventStage,
    pub recovery: bool,
    pub program: NativeEventProgram,
}

impl NativePreflightContext {
    pub(super) fn event(event: &NativeTransactionEvent, root: &Path, recovery: bool) -> Self {
        Self {
            package: event.owner_package.clone(),
            version: event.owner_version.clone(),
            architecture: event.owner_arch.clone(),
            source_format: event.source_format.clone(),
            root: root.into(),
            stage: event.stage,
            recovery,
            program: event.program.clone(),
        }
    }
}
