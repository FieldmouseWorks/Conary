//! A failed gate's second try (`docs/DESIGN.md` section 4.7). A gate that
//! died says what a failing test says (a full disk is an exit of 1 too), so
//! no failure is red on its first try. GitHub's run attempt is the only
//! record of the try, so a step run again asks for nothing twice.

use std::collections::BTreeMap;

use conary_core::op::Repo;

use super::gate::{Failure, Gate};
use super::{Forge, TrainError};
use crate::api::{CheckRun, Source, overran};

/// What a gate with failures says once their second tries count: waiting
/// while a try is asked or going, red once a failure can have no other.
/// The whole workflow run is run again, never only its failed jobs: GitHub
/// gives the copies of the others new ids, which their attestations don't
/// name. A check cancelled or stale with no try left isn't red either: it
/// waits, a stall to drop. One past its timeout is red as the flow reads it,
/// so a candidate a change hangs still splits down to that change.
pub(super) fn second_try(
    forge: &dyn Forge,
    source: &dyn Source,
    repo: &Repo,
    failures: Vec<Failure>,
) -> Result<Gate, TrainError> {
    let full = repo.to_string();
    // Each failure, with the workflow run to run again if it has a try left.
    let mut tries = Vec::new();
    for failure in &failures {
        // A status has nothing to run again.
        let Some(run) = &failure.run else {
            return Ok(Gate::Red(failures));
        };
        let left = forge
            .job(&full, run.id)?
            .filter(|job| job.run_attempt == 1)
            .map(|job| job.run_id);
        if left.is_none() && !no_verdict(source, &full, run)? {
            return Ok(Gate::Red(failures));
        }
        tries.push((run, left));
    }
    let mut asked = BTreeMap::new();
    let mut waiting = Vec::new();
    for (run, left) in tries {
        let going = match left {
            None => false,
            Some(id) => match asked.get(&id) {
                Some(&going) => going,
                None => {
                    let going = ask(forge, &full, id)?;
                    asked.insert(id, going);
                    going
                }
            },
        };
        waiting.push(if going {
            format!("{}: it failed, and gets a second try", run.name)
        } else if no_verdict(source, &full, run)? {
            format!("{}: it ended with no verdict and no try left", run.name)
        } else {
            return Ok(Gate::Red(failures));
        });
    }
    Ok(Gate::Waiting(waiting))
}

/// Whether the run ended saying nothing about the change.
fn no_verdict(source: &dyn Source, repo: &str, run: &CheckRun) -> Result<bool, TrainError> {
    Ok(match run.conclusion.as_deref() {
        Some("cancelled") => !overran(&source.annotations(repo, run.id)?),
        Some("stale") => true,
        _ => false,
    })
}

/// Sees that workflow run `id`, which holds a first attempt's failure, has
/// its second: asked once the first has finished. False when GitHub won't
/// run it again.
fn ask(forge: &dyn Forge, repo: &str, id: u64) -> Result<bool, TrainError> {
    if !forge.workflow_run(repo, id)?.first_finished() {
        return Ok(true);
    }
    match forge.rerun(repo, id) {
        Ok(()) => Ok(true),
        Err(e) if e.unavailable => Err(e.into()),
        // Refused as already running, when another asked first.
        Err(_) => Ok(!forge.workflow_run(repo, id)?.first_finished()),
    }
}
