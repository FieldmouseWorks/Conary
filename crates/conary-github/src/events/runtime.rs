//! Machine-owned configuration and the shared pass entry, without shell commands.
use super::{Queue, Work};
use crate::{
    Api, Tokens, anchor,
    flow::{self, persist::Result},
};
use conary_core::{Policy, log, reader};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// conaryd's events configuration file. A running daemon reads it again at
/// each delivery and pass, so every option but those declared `x-live: false`
/// takes effect at the next one.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    /// The address the webhook listener binds.
    #[schemars(extend(
        "x-spender" = false,
        "x-live" = false,
        "x-needs" = "a restart of conaryd: it binds the address once, as it starts"
    ))]
    pub listen: std::net::SocketAddr,
    /// The file holding the webhook secret's exact bytes.
    #[schemars(extend(
        "x-spender" = false,
        "x-live" = false,
        "x-needs" = "a restart of conaryd: it reads the secret once, as it starts"
    ))]
    pub secret_path: PathBuf,
    /// The repositories whose events start passes, as `OWNER/REPO`.
    #[serde(deserialize_with = "repos", serialize_with = "repo_names")]
    #[schemars(with = "Vec<String>", extend("x-spender" = false))]
    pub repos: Vec<conary_core::op::Repo>,
    /// The directory of the mailbox, the delivery record and the flow's state.
    #[schemars(extend(
        "x-spender" = false,
        "x-live" = false,
        "x-needs" = "a restart of conaryd: it opens its mailbox there once, as it starts"
    ))]
    pub data: PathBuf,
    /// The flow's configuration file.
    #[schemars(extend("x-spender" = false))]
    pub flow_config: PathBuf,
    /// The classification policy file.
    #[schemars(extend("x-spender" = false))]
    pub policy: PathBuf,
    /// The command that prints a GitHub token, run without a shell.
    #[schemars(extend("x-spender" = false))]
    pub token_command: String,
    /// The repository the ledger is anchored in. `null` only when the flow
    /// needs no evidence or train.
    #[schemars(extend("x-spender" = false))]
    pub anchor_repo: Option<String>,
    /// The installed `cn` that runs the flow's plugin jobs.
    #[schemars(extend("x-spender" = false))]
    pub worker: PathBuf,
    /// The GitHub API root.
    #[serde(default = "api")]
    #[schemars(extend("x-spender" = false))]
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
fn repo_names<S: serde::Serializer>(
    repos: &[conary_core::op::Repo],
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    s.collect_seq(repos.iter().map(ToString::to_string))
}

/// The events configuration as a running daemon reads it: again at each
/// delivery and pass, with the options the schema declares `x-live: false`
/// kept as the daemon started with them. A change to one of those waits for
/// a restart, and is logged once, by its name.
pub struct Live {
    path: PathBuf,
    started: serde_json::Value,
    runtime: Runtime,
    /// The options the schema declares `x-live: false`.
    restart: Vec<String>,
    waiting: Mutex<Vec<String>>,
}
impl Live {
    pub fn open(path: &Path) -> Result<Self> {
        let started: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
        Ok(Self {
            path: path.into(),
            runtime: serde_json::from_value(started.clone())?,
            started,
            restart: flow::schema::restart_only::<Runtime>(),
            waiting: Mutex::default(),
        })
    }
    /// The configuration the daemon started with.
    pub fn started(&self) -> &Runtime {
        &self.runtime
    }
    /// The configuration now: the file as it reads, but for the options
    /// that take effect only at a restart.
    pub fn read(&self) -> Result<Runtime> {
        let mut now: serde_json::Value = serde_json::from_slice(&std::fs::read(&self.path)?)?;
        let mut waiting = Vec::new();
        if let Some(fields) = now.as_object_mut() {
            for name in &self.restart {
                let started = self.started.get(name);
                if fields.get(name) != started {
                    waiting.push(name.clone());
                }
                match started {
                    Some(value) => fields.insert(name.clone(), value.clone()),
                    None => fields.remove(name),
                };
            }
        }
        let runtime = serde_json::from_value(now)?;
        let mut noted = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
        if *noted != waiting {
            if !waiting.is_empty() {
                eprintln!(
                    "conaryd: {} changed in {}: conaryd keeps what it started with until it restarts",
                    waiting.join(", "),
                    self.path.display()
                );
            }
            *noted = waiting;
        }
        Ok(runtime)
    }
    /// The options changed in the file that wait for a restart, as the last
    /// read saw them.
    pub fn waiting(&self) -> Vec<String> {
        self.waiting
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    /// The repositories listed now, as the receiver takes them.
    pub fn repos(&self) -> Result<Vec<String>> {
        Ok(self.read()?.repos.iter().map(ToString::to_string).collect())
    }
    /// One work item: a pass of its repository, under the configuration as
    /// it reads now. A repository no longer listed retires its mailbox.
    pub fn work(
        &self,
        ledger: &Path,
        work: &Work,
        queue: &std::sync::Arc<Queue>,
    ) -> Result<Vec<flow::Report>> {
        let runtime = self.read()?;
        if !runtime
            .repos
            .iter()
            .any(|r| r.to_string().eq_ignore_ascii_case(&work.repo))
        {
            return Ok(vec![]);
        }
        runtime.pass(ledger, work, queue)
    }
    /// The worker: a failed pass books its repository's recovery and the
    /// rest go on. Only the mailbox's own errors stop it. The log names the
    /// repository and its retry, never an error's text, which no webhook
    /// reaches but a token command's output might.
    pub fn run(
        &self,
        ledger: &Path,
        queue: &std::sync::Arc<Queue>,
    ) -> Result<std::convert::Infallible> {
        self.runtime.restart(queue)?;
        queue.work(
            |work| self.work(ledger, work, queue),
            |repo, recovery| {
                eprintln!(
                    "conaryd: a flow pass of {repo} failed ({} in a row); it runs again at {}, its kicks and bookings kept",
                    recovery.attempts, recovery.at
                );
            },
        )
    }
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
        let config = flow::Config::load(&self.flow_config)?;
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
        let process = self.launcher(&work.repo, queue);
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
    /// Launches this repository's workers, and kicks it again as each one
    /// exits, so the next pass reads its answer, its failure or its
    /// cancellation without waiting for GitHub or a deadline.
    pub fn launcher(&self, repo: &str, queue: &std::sync::Arc<Queue>) -> flow::jobs::Process {
        flow::jobs::Process {
            worker: self.worker.clone(),
            ended: Some(ended(repo, queue)),
        }
    }
    /// Kicks every repository once, as the daemon starts, so a worker that
    /// answered while no daemon ran, or before its exit was kicked, is read
    /// by that first pass. Then it watches each worker with no answer yet,
    /// one an earlier daemon launched that may still run, and kicks its
    /// repository as it exits, however it ended, its cancellation by this
    /// daemon included.
    pub fn restart(&self, queue: &std::sync::Arc<Queue>) -> Result<()> {
        for repo in &self.repos {
            queue.kick(super::Kick {
                repo: repo.to_string(),
                pr: None,
            })?;
        }
        for repo in &self.repos {
            let watched =
                flow::jobs::outstanding(&flow::persist::dir(&self.data, repo)).and_then(|jobs| {
                    jobs.iter()
                        .try_for_each(|job| flow::jobs::watch(job, ended(&repo.to_string(), queue)))
                });
            if watched.is_err() {
                eprintln!(
                    "conaryd: the flow workers {repo} had running can't all be watched; their deadlines are still booked"
                );
            }
        }
        Ok(())
    }
}
/// Kicks `repo` once a worker of it has exited. The kick is the mailbox's,
/// durable once made; a worker's deadline stays booked as the backstop for
/// one whose exit nobody saw.
fn ended(repo: &str, queue: &std::sync::Arc<Queue>) -> flow::jobs::Ended {
    let queue = std::sync::Arc::clone(queue);
    let repo = repo.to_owned();
    std::sync::Arc::new(move || {
        let kick = super::Kick {
            repo: repo.clone(),
            pr: None,
        };
        if queue.kick(kick).is_err() {
            eprintln!(
                "conaryd: a flow worker of {repo} ended, but its mailbox can't be written; its deadline is still booked"
            );
        }
    })
}
