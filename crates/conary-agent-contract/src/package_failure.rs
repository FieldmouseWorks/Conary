// crates/conary-agent-contract/src/package_failure.rs
//! Package failure observations; these reports never authorize mutation or retry.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum PackageFailureSchema {
    #[serde(rename = "conary.package.failure.v1")]
    V1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PackageFailureReport {
    pub schema: PackageFailureSchema,
    /// Present only for the update selection whose failures were aggregated.
    pub requested_updates: Option<usize>,
    /// None means commit observations were not supplied by this operation.
    /// An empty list never establishes that an enclosing operation changed nothing.
    pub committed_changesets: Option<Vec<i64>>,
    pub failures: Vec<PackageFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PackageFailure {
    pub package: String,
    pub version: String,
    pub native_preflight: Option<NativePreflightFailure>,
    /// Full error chain when no native preflight boundary was observed.
    pub causes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NativePreflightFailure {
    /// Event owner, which can differ from the requested package (dependencies,
    /// relation removals, triggers, or recovery).
    pub package: String,
    pub version: String,
    pub architecture: Option<String>,
    pub source_format: String,
    /// Operator-selected scope, when supplied by the command boundary.
    pub requested_root: Option<String>,
    pub database: Option<String>,
    /// Materialized transaction root actually checked; it may be temporary.
    pub execution_root: String,
    pub stage: String,
    pub program: NativePreflightProgram,
    pub cause: NativePreflightCause,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativePreflightProgram {
    BundleEntry { entry_id: String },
    Command { argv: Vec<String> },
    RpmSysusers { source_path: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativePreflightCause {
    MissingInterpreter {
        interpreter: String,
        entry_id: String,
        projected: bool,
    },
    InvalidExecutionRoot {
        root: String,
    },
    TimeoutOutOfRange {
        entry_id: String,
        timeout_ms: u64,
        minimum_ms: u64,
        maximum_ms: u64,
    },
    /// Unclassified errors retain their chain; no remediation is inferred from text.
    Unclassified {
        causes: Vec<String>,
    },
}

#[cfg(test)]
mod tests;
