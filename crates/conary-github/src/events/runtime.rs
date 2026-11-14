//! Machine-owned configuration and the shared pass entry, without shell commands.
use super::{Queue, Work};
use crate::{
    Api, Tokens, anchor,
    flow::{self, persist::Result},
};
use conary_core::{Policy, log, reader};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    pub listen: std::net::SocketAddr,
    pub secret_path: PathBuf,
    #[serde(deserialize_with = "repos")]
    pub repos: Vec<conary_core::op::Repo>,
    pub data: PathBuf,
    pub flow_config: PathBuf,
    pub policy: PathBuf,
    pub token_command: String,
    pub anchor_repo: Option<String>,
    pub worker: PathBuf,
    #[serde(default = "api")]
    pub api: String,
}
fn api() -> String {
    "https://api.github.com".into()
}
fn repos<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Vec<conary_core::op::Repo>, D::Error> {
    Vec::<String>::deserialize(d)?
        .into_iter()
        .map(|s| s.parse().map_err(serde::de::Error::custom))
        .collect()
}
impl Runtime {
    /// This is the same Pass::run used by cn flow kick. Never accept a path,
    /// executable or policy from a webhook payload.
    pub fn pass(
        &self,
        ledger: &std::path::Path,
        work: &Work,
        queue: &std::sync::Arc<Queue>,
    ) -> Result<Vec<flow::Report>> {
        let repo = self
            .repos
            .iter()
            .find(|r| r.to_string().eq_ignore_ascii_case(&work.repo))
            .ok_or("unlisted repository")?;
        let config: flow::Config = serde_json::from_slice(&std::fs::read(&self.flow_config)?)?;
        let policy = Policy::load(&self.policy)?;
        let api = Api::new(&self.api, Tokens::command(&self.token_command)?)
            .with_event_queue(std::sync::Arc::clone(queue));
        let needs_ledger =
            config.settings.train || config.evidence.is_some() || self.anchor_repo.is_some();
        let mut anchored = if needs_ledger {
            Some(anchor::open(
                ledger,
                log::load_signing_key(&log::key_path(ledger))?,
                &api,
                self.anchor_repo
                    .as_deref()
                    .ok_or("flow evidence and train operations require anchor_repo")?,
            )?)
        } else {
            None
        };
        let observed = if anchored.is_none() {
            Some(reader::read(
                ledger,
                &log::load_public_key(&log::public_path(&log::key_path(ledger)))?,
            )?)
        } else {
            None
        };
        let process = flow::jobs::Process(self.worker.clone());
        let pass = flow::Pass {
            config: &config,
            policy: &policy,
            root: &self.data,
            launcher: &process,
            now: flow::jobs::now(),
            dry_run: false,
            ledger: observed.as_ref(),
        };
        let reports = pass.run(&api, repo, None, anchored.as_mut().map(|a| &mut a.store));
        if let Some(anchored) = &mut anchored {
            anchored.publish(&api)?;
        }
        reports
    }
    /// The worker: a failed pass books its repository's recovery and the
    /// rest go on. Only the mailbox's own errors stop it. The log names the
    /// repository and its retry, never an error's text, which no webhook
    /// reaches but a token command's output might.
    pub fn run(
        &self,
        ledger: &std::path::Path,
        queue: &std::sync::Arc<Queue>,
    ) -> Result<std::convert::Infallible> {
        queue.work(
            |work| {
                if !self
                    .repos
                    .iter()
                    .any(|r| r.to_string().eq_ignore_ascii_case(&work.repo))
                {
                    // Removing a repo from config retires its old mailbox.
                    return Ok(vec![]);
                }
                self.pass(ledger, work, queue)
            },
            |repo, recovery| {
                eprintln!(
                    "conaryd: a flow pass of {repo} failed ({} in a row); it runs again at {}, its kicks and bookings kept",
                    recovery.attempts, recovery.at
                );
            },
        )
    }
}
