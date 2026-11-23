//! Each read of GitHub goes to the end of its data, page by page, as the
//! simulated GitHub serves it (FP9 of `docs/m6-properties.md`): a head with
//! more check suites than one query holds, and more pages of check runs than
//! any fixed cap, is read in full and its PR lands on the real checks.
use super::flow_pass_case::{Case, REPO};
use super::head;
use crate::api::Source;
use crate::flow::*;
use crate::sim::Call;

/// Pages of check runs past the fifty the pager once stopped at.
const RUNS: usize = 51 * crate::api::PER_PAGE;

#[test]
fn a_head_with_thousands_of_suites_and_runs_is_read_in_full_and_lands() {
    let c = Case::new();
    let now = jobs::now() + 10_000;
    c.pass(now - 1, false);
    c.clean_review();
    // Every run its own suite: more suites than a query's page, and more
    // pages of runs than fifty.
    for i in c.hub.checks_on(&head(3)).len()..RUNS {
        c.hub
            .check_run(&head(3), &format!("shard {i}"), "success", None);
    }
    let reports = c.pass(now, false);
    let three = reports.iter().find(|r| r.pr == 3).unwrap();
    let decided = format!("{three:?}");
    assert!(three.error.is_none(), "{decided}");
    assert!(c.hub.pull(REPO, 3).unwrap().is_merged(), "{decided}");
    // Every page of runs and of suites read, each its own call.
    let pages = RUNS / crate::api::PER_PAGE;
    assert!(c.hub.count(Call::CheckRuns) >= pages);
    assert!(c.hub.count(Call::CheckSuites) >= pages - 1);
}
