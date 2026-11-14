//! Trusted CI orchestration. Git supplies object identities; no command in
//! this module executes anything from the candidate working tree.
use std::path::PathBuf;

use clap::Args;
use serde_json::json;

use super::{AttestCommand, GateArgs, Out};
use crate::{Cli, Result};
use conary_github::attest::ci;

#[derive(Args)]
pub struct RevisionArgs {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    base: String,
    #[arg(long)]
    head: String,
}

#[derive(Args)]
pub struct CallerArgs {
    /// GitHub's caller repository, not a workflow input.
    #[arg(long)]
    repository: String,
    /// GitHub's caller workflow identity, not a workflow input.
    #[arg(long)]
    workflow_ref: String,
    /// The repository's default branch, supplied by GitHub's event.
    #[arg(long)]
    default_branch: String,
}

#[derive(Args)]
pub struct CiArgs {
    #[command(flatten)]
    caller: CallerArgs,
    #[arg(long)]
    repo: PathBuf,
    /// The exact commit checked out by the caller's test jobs.
    #[arg(long)]
    commit: String,
    #[arg(long, requires = "head")]
    base: Option<String>,
    #[arg(long, requires = "base")]
    head: Option<String>,
    /// toJSON(needs) from the caller; names and conclusions go in the hashed log.
    #[arg(long)]
    jobs: String,
    /// Sign the negative job instead of the gate. Missing results fail closed.
    #[arg(long, requires = "base")]
    negative_result: Option<String>,
    /// The caller command whose result the negative job reports.
    #[arg(
        long,
        requires = "negative_result",
        required_if_eq("negative_result", "fail"),
        required_if_eq("negative_result", "pass")
    )]
    negative_check: Option<String>,
    #[command(flatten)]
    out: Out,
}

pub fn prepare(args: &RevisionArgs) -> Result<()> {
    let plan = ci::plan(&args.repo, &args.base, &args.head)?;
    println!(
        "{}",
        json!({"base": plan.base, "tree": plan.tree, "tests": plan.tests})
    );
    Ok(())
}

pub fn attest(cli: &Cli, args: &CiArgs) -> Result<()> {
    ci::trusted_caller(
        &args.repo,
        &args.commit,
        &args.caller.repository,
        &args.caller.workflow_ref,
        &args.caller.default_branch,
    )?;
    let (result, summary) = ci::summary(&args.jobs)?;
    let tree = ci::tree(&args.repo, &args.commit, args.head.as_deref())?;
    let command = if let (Some(base), Some(head)) = (&args.base, &args.head) {
        let rev = RevisionArgs {
            repo: args.repo.clone(),
            base: base.clone(),
            head: head.clone(),
        };
        let plan = ci::plan(&rev.repo, &rev.base, &rev.head)?;
        if let Some(negative_result) = &args.negative_result {
            if negative_result.is_empty() || result != super::Gate::Pass {
                return Err("negative execution must have completed successfully".into());
            }
            let result = match (plan.tree.is_some(), negative_result.as_str()) {
                (false, "none") => super::Negative::None,
                (true, "fail") => super::Negative::Fail,
                (true, "pass") => super::Negative::Pass,
                _ => {
                    return Err(
                        "negative execution did not report a result matching its tree".into(),
                    );
                }
            };
            AttestCommand::Negative {
                base: plan.base,
                head: head.clone(),
                result: Some(result),
                tree: plan.tree,
                tests: plan.tests,
                checks: if result == super::Negative::None {
                    vec![]
                } else {
                    vec![
                        args.negative_check
                            .clone()
                            .ok_or("negative needs its test command")?,
                    ]
                },
                skipped: vec![],
                unrun: vec![],
                from: None,
                out: copy_out(&args.out),
            }
        } else {
            AttestCommand::Revision {
                base: plan.base,
                head: head.clone(),
                gate: GateArgs {
                    result,
                    tree,
                    skipped: vec![],
                },
                out: copy_out(&args.out),
            }
        }
    } else {
        AttestCommand::Commit {
            commit: args.commit.clone(),
            gate: GateArgs {
                result,
                tree,
                skipped: vec![],
            },
            out: copy_out(&args.out),
        }
    };
    let log = match (&args.out.log, args.out.leg_logs.is_empty()) {
        (Some(log), true) => log,
        _ => {
            return Err(
                "cn attest ci writes its one summary log: give --log, not --leg-log".into(),
            );
        }
    };
    std::fs::write(log, format!("{summary}\n"))?;
    super::run(cli, &command)
}

fn copy_out(out: &Out) -> Out {
    Out {
        log: out.log.clone(),
        leg_logs: out.leg_logs.clone(),
        log_artifact: out.log_artifact.clone(),
        out: out.out.clone(),
        sign: out.sign,
    }
}
