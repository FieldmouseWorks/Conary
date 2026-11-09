//! One pass of the landing flow. Reading and deciding are separate: [`next`]
//! has no clock or effects; its returned step is an intention, never a merge.
//! Explicit CLI passes execute intentions through GitHub and plugin traits.

mod ci;
mod claims;
pub use claims::{FixAnswer, FixClaim, FixResult, Unposted};
mod decide;
mod gates;
mod history;
mod markers;
mod proofs;
mod read;
mod state;
pub use proofs::{
    Derivation, Evaluation, InputBuild, ProofData, ProofHolder, ProofPlan, ProofRecord,
    ProofWaiting,
};
mod review;
pub(crate) mod reviews;
mod train;
mod types;
pub use train::{Candidate, Seat, TrainState};

pub use ci::{Suite, Workflow};
pub use markers::SealKey;
pub use read::{BaseObservation, Read, Snapshot, read};
pub use reviews::fingerprint;
pub use state::next;
pub use types::*;

mod persist;
pub use persist::{Data, Record};
pub mod jobs;

mod write;
pub use write::{SETTLED, Write, review_body};

mod execute;
mod observe;
mod pass;
pub use pass::{Config, Pass, Plugins, Report};

mod evidence;
pub use evidence::{Evidence, judgment_receipt, major_receipt, receipt_digest, review_receipt};

mod scope;

mod occupancy;
