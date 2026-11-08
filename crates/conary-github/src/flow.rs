//! One pass of the landing flow. Reading and deciding are separate: [`next`]
//! has no clock or effects; its returned step is an intention, never a merge.
//! Nothing in the daemon or CLI executes these intentions yet.

mod ci;
mod decide;
mod gates;
mod history;
mod markers;
mod proofs;
mod read;
pub use proofs::{
    Derivation, Evaluation, ProofData, ProofHolder, ProofPlan, ProofRecord, ProofWaiting,
};
mod review;
pub(crate) mod reviews;
mod types;

pub use ci::{Suite, Workflow};
pub use decide::next;
pub use markers::SealKey;
pub use read::{Read, Snapshot, read};
pub use reviews::fingerprint;
pub use types::*;
