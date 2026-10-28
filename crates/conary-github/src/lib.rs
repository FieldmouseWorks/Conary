//! Conary's GitHub side: reading pull requests, their checks and reviews,
//! and recording them in the ledger. All it writes on GitHub is the ledger's
//! anchor and each repository's notes.

pub mod anchor;
pub mod api;
pub mod bases;
pub mod ingest;
pub mod notes;

pub use api::{Api, Source};
pub use ingest::{Config, IngestError, Ingested};
