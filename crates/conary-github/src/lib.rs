//! Conary's GitHub side: reading pull requests, their checks and reviews,
//! and recording them in the ledger. Read-only toward GitHub.

pub mod anchor;
pub mod api;
pub mod ingest;

pub use api::{Api, Source};
pub use ingest::{Config, IngestError, Ingested};
