//! Conary's GitHub side: reading pull requests, their checks and reviews,
//! and recording them in the ledger. The only thing it writes on GitHub is
//! the ledger's anchor.

pub mod anchor;
pub mod api;
pub mod bases;
pub mod ingest;

pub use api::{Api, Source};
pub use ingest::{Config, IngestError, Ingested};
