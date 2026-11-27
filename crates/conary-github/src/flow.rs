//! One pass of the landing flow. Reading and deciding are separate: [`next`]
//! has no clock or effects; its returned step is an intention, never a merge.
//! Explicit CLI passes execute intentions through GitHub and plugin traits.

mod ci;
mod claims;
pub use claims::{FixAnswer, FixClaim, FixResult, Unposted};
mod decide;
pub(crate) mod deferred;
pub use deferred::{Deferral, DeferredConfig, DeferredFailure, Dispatch, Start};
pub mod durations;
mod gates;
mod history;
mod markers;
mod priority;
pub use degraded::{Degradation, Degraded, Mark, ReadError, Reading, Reads};
pub use priority::{DEFAULT_RED_FIX_BRANCH, Level, Rank, StallAfter, red_fix};
mod proofs;
mod read;
mod recover;
pub use recover::{Answer, Recovered, Recovery, RerunPolicy};
mod uncertain;
pub use uncertain::UncertainCheck;
#[cfg(test)]
pub(crate) use uncertain::hydrate as hydrate_uncertain_for_test;
mod state;
#[cfg(test)]
pub(crate) use approval::labeller;
#[cfg(test)]
pub(crate) use proofs::selected;
pub use proofs::{
    Preparation, ProofData, ProofHolder, ProofPlan, ProofRecord, ProofWaiting, Reached, Readiness,
};
mod review;
#[cfg(test)]
pub(crate) use review::QUIET;
pub(crate) mod reviews;
mod train;
mod types;
pub use train::{Candidate, Seat, TrainState};

pub use ci::{Suite, Workflow};
pub use markers::SealKey;
pub use read::{BaseObservation, Read, Snapshot, read};
pub use reviews::{CustomReviewAdapter, ReviewAdapter, Since, fingerprint};
pub use state::next;
pub use types::*;

pub(crate) mod persist;
pub use persist::{Data, Landing, Record};
pub mod jobs;

mod write;
pub use write::{SETTLED, StageLabels, Write, review_body};

mod ask;
mod execute;
#[cfg(test)]
pub(crate) use execute::act as execute_for_test;
pub(crate) mod observe;
mod pass;
pub mod schema;
pub use ask::ReviewAsk;
pub use pass::{Config, Pass, Plugins, Report};

mod judging;
pub use judging::{judging_context, read_judging_context, review_bodies};

mod compare;
pub use compare::{Beside, Decided, Said, Tally, beside};

pub(crate) mod evidence;
pub use evidence::{
    Evidence, judgment_receipt, judgment_receipt_by, major_receipt, receipt_digest, recipe_digest,
};

mod scope;

pub(crate) mod degraded;
mod occupancy;

pub mod pause;
pub use pause::{Pause, Scope};

pub(crate) mod switches;
pub use switches::{DegradedPr, Held, Holding, InEffect, Status, Switch, status};

pub(crate) mod ledger;

mod approval;
pub use approval::{Door, Labeller, Request, Subject, owner_approval};

mod handoff;
mod stuck;
pub use stuck::{Disputed, ENDED as STUCK_ENDED, Handoff, Stuck, followup as stuck_followup};

pub(crate) mod publish;
