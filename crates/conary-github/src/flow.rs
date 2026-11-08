//! One pass of the landing flow. Reading and deciding are separate: [`next`]
//! has no clock or effects; its returned step is an intention, never a merge.
//! Nothing in the daemon or CLI executes these intentions yet.

mod ci;
mod decide;
mod read;
mod types;

pub use ci::{Suite, Workflow};
pub use decide::next;
pub use read::{Read, Snapshot, read};
pub use types::*;
