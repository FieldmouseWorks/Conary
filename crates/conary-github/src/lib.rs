//! Conary's GitHub side: reading pull requests, their checks and reviews,
//! and recording them in the ledger. All it writes on GitHub is the ledger's
//! anchor, each repository's notes, the train's candidates on their own
//! branches, and undo's reverts, each on its own branch and PR.

pub mod anchor;
pub mod api;
pub mod attest;
pub mod bases;
pub mod branches;
pub mod class;
pub mod etags;
pub mod ingest;
pub mod notes;
#[cfg(test)]
pub(crate) mod sim;
pub mod tokens;
pub mod train;
pub mod undo;

pub use api::{Api, ApiError, Source};
pub use etags::Etags;
pub use ingest::{Config, IngestError, Ingested};
pub use tokens::Tokens;
