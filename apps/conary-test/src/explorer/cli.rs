// apps/conary-test/src/explorer/cli.rs

use super::{
    contract::*,
    controller::{self, Campaign, Episode},
    evidence::{Identity, Replay},
    selector::{Scripted, Seeded, Selector},
};
use anyhow::{Result, ensure};
use clap::Subcommand;
use std::path::PathBuf;
use std::sync::{Arc, atomic::AtomicBool};

#[derive(Subcommand)]
pub enum ExplorerCommands {
    /// Build inert local CCS fixture packages; performs no package installation
    Fixtures {
        #[arg(long)]
        output: PathBuf,
    },
    /// Run inside an explicitly approved disposable VM
    Run {
        #[arg(long)]
        approved_guest: PathBuf,
        #[arg(long)]
        fixtures: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        calibration: bool,
        /// Seek unvisited package states using deterministic typed action projections
        #[arg(long, conflicts_with_all = ["calibration", "seed", "jev_mock", "jev_live"])]
        coverage_greedy: bool,
        #[arg(long, default_value_t = 7)]
        seed: u64,
        /// Attempted-action ceiling for this episode (1..=64), including stop choices
        #[arg(long, default_value_t = 64, value_parser = clap::value_parser!(u32).range(1..=64))]
        max_actions: u32,
        /// Optional local mock transport; never contacts a live model
        #[arg(long, conflicts_with = "calibration")]
        jev_mock: Option<String>,
        /// Contact the live TypeSafe API (paid); requires TYPESAFE_API_KEY
        #[arg(long, conflicts_with_all = ["calibration", "jev_mock"])]
        jev_live: bool,
        /// Live HTTP request limit including retries (1..=8)
        #[arg(long, default_value_t = 8, requires = "jev_live")]
        jev_max_requests: u32,
        /// HTTP attempts per decision, still within the total request limit (1..=3)
        #[arg(long, default_value_t = 2, requires = "jev_live", value_parser = clap::value_parser!(u32).range(1..=3))]
        jev_max_attempts: u32,
    },
    /// Execute saved concrete operations, without a selector
    Replay {
        #[arg(long)]
        approved_guest: PathBuf,
        #[arg(long)]
        fixtures: PathBuf,
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Remove steps while preserving the recorded failure predicate
    Reduce {
        #[arg(long)]
        approved_guest: PathBuf,
        #[arg(long)]
        fixtures: PathBuf,
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
}

impl ExplorerCommands {
    pub async fn execute(self) -> Result<()> {
        if let Self::Fixtures { output } = self {
            println!(
                "{}",
                serde_json::to_string_pretty(&super::fixtures::build(&output)?)?
            );
            return Ok(());
        }
        let (
            approved_guest,
            fixtures,
            output,
            calibration,
            coverage_greedy,
            seed,
            max_actions,
            jev_mock,
            jev_live,
            jev_max_requests,
            jev_max_attempts,
            input,
            reduce,
        ) = match self {
            Self::Run {
                approved_guest,
                fixtures,
                output,
                calibration,
                coverage_greedy,
                seed,
                max_actions,
                jev_mock,
                jev_live,
                jev_max_requests,
                jev_max_attempts,
            } => (
                approved_guest,
                fixtures,
                output,
                calibration,
                coverage_greedy,
                seed,
                max_actions,
                jev_mock,
                jev_live,
                jev_max_requests,
                jev_max_attempts,
                None,
                false,
            ),
            Self::Replay {
                approved_guest,
                fixtures,
                input,
                output,
            } => (
                approved_guest,
                fixtures,
                output,
                false,
                false,
                0,
                64,
                None,
                false,
                0,
                1,
                Some(input),
                false,
            ),
            Self::Reduce {
                approved_guest,
                fixtures,
                input,
                output,
            } => (
                approved_guest,
                fixtures,
                output,
                false,
                false,
                0,
                64,
                None,
                false,
                0,
                1,
                Some(input),
                true,
            ),
            Self::Fixtures { .. } => unreachable!(),
        };
        let approval = super::sandbox::GuestApproval::load(&approved_guest)?;
        approval.verify_here()?;
        let backend = crate::container::BollardBackend::new()?;
        let mut environment =
            super::conary::ConaryEnvironment::new(&backend, approval.clone(), fixtures)?;
        let identity = Identity {
            source_revision: approval.source_revision,
            conary_sha256: approval.conary_sha256,
            image: approval.image,
            fixtures: environment.fixture_hashes.clone(),
        };
        let replay = input.as_ref().map(|p| Replay::load(p)).transpose()?;
        if let Some(replay) = &replay {
            ensure!(
                digest(&identity)? == digest(&replay.identity)?,
                "replay artifact identity mismatch"
            );
        }
        let mut campaign = Campaign::new(Limits {
            actions: max_actions,
            ..Limits::default()
        })?;
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let signal = tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        });
        let result = async {
            if reduce {
                let result = super::reducer::reduce(
                    &mut environment,
                    &mut campaign,
                    replay.as_ref().expect("reduce has input"),
                    &output,
                    &cancel,
                )
                .await?;
                println!("{}", serde_json::to_string_pretty(&result)?);
                return Ok(());
            }
            let mode = if replay.is_some() {
                Mode::Replay
            } else if calibration {
                Mode::Calibration
            } else {
                Mode::Exploration
            };
            let mut selector: Box<dyn Selector> = if let Some(url) = jev_mock {
                Box::new(super::jev::Jev::mock(
                    &url,
                    64,
                    2,
                    std::time::Duration::from_secs(3),
                    cancel.clone(),
                )?)
            } else if jev_live {
                let key = std::env::var("TYPESAFE_API_KEY")
                    .map_err(|_| anyhow::anyhow!("live Jev requires TYPESAFE_API_KEY"))?;
                Box::new(super::jev::Jev::live(
                    &key,
                    jev_max_requests,
                    jev_max_attempts,
                    cancel.clone(),
                )?)
            } else if calibration {
                Box::new(Scripted(super::selector::calibration()))
            } else if coverage_greedy {
                Box::new(super::coverage::CoverageGreedy)
            } else {
                Box::new(Seeded::new(seed))
            };
            let report = controller::run(
                &mut environment,
                if replay.is_some() {
                    None
                } else {
                    Some(selector.as_mut())
                },
                &mut campaign,
                Episode {
                    mode,
                    identity,
                    replay: replay.as_ref(),
                    output: &output,
                    cancel: &cancel,
                },
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            ensure!(
                report.reproduces_recorded_predicate != Some(false)
                    && report.cleanup == "removed"
                    && report.reset_verified
                    && !report.evaluations.iter().any(|e| matches!(
                        e.classification,
                        Classification::HarnessFailure
                            | Classification::EnvironmentFailure
                            | Classification::Inconclusive
                            | Classification::ProductFailure
                    )),
                "experiment did not pass; inspect report"
            );
            Ok(())
        }
        .await;
        signal.abort();
        result
    }
}
