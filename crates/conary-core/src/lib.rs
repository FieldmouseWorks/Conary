//! Conary's core model, as `docs/DESIGN.md` section 4 describes it. All of
//! Conary's logic lives in this crate; conaryd, the MCP server and the `cn`
//! CLI are thin layers over it.

pub mod id;

pub use id::{ChangeId, CommitId, ParseIdError, RevisionId};
