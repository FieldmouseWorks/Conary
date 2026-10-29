//! Conary's GitHub side: reading pull requests, their checks and reviews,
//! and recording them in the ledger. All it writes on GitHub is the ledger's
//! anchor and each repository's notes.

pub mod anchor;
pub mod api;
pub mod bases;
pub mod etags;
pub mod ingest;
pub mod notes;
pub mod tokens;

pub use api::{Api, ApiError, Source};
pub use etags::Etags;
pub use ingest::{Config, IngestError, Ingested};
pub use tokens::Tokens;
