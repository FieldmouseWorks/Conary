//! Conary's core model, as `docs/DESIGN.md` section 4 describes it. All of
//! Conary's logic lives in this crate; conaryd, the MCP server and the `cn`
//! CLI are thin layers over it.

pub mod git;
pub mod id;
pub mod ledger;
pub mod log;
pub mod notes;
pub mod op;
pub mod store;
pub mod why;

pub use id::{ChangeId, CommitId, OpId, ParseIdError, RevisionId};
pub use ledger::Ledger;
pub use store::Store;
