// apps/conary-test/src/explorer/conary.rs

use super::{contract::*, controller::Environment, fixtures, sandbox::GuestApproval};
use crate::config::manifest::Assertion;
use crate::container::{ContainerBackend, ContainerConfig, VolumeMount};
use crate::engine::assertions::evaluate_assertion;
use anyhow::{Result, ensure};
use async_trait::async_trait;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::Duration;

const ROOT: &str = "/work/root";
const DB: &str = "/work/root/var/lib/conary/conary.db";

/// Uses the existing container lifecycle and argv execution authority.
/// Only registered inert fixture operations reach this adapter.
pub struct ConaryEnvironment<'a> {
    pub backend: &'a dyn ContainerBackend,
    pub approval: GuestApproval,
    pub fixtures: PathBuf,
    pub fixture_hashes: BTreeMap<String, String>,
    container: Option<String>,
    scratch: Option<tempfile::TempDir>,
    epoch: u64,
    revision: u64,
    submitted: BTreeSet<String>,
}
impl<'a> ConaryEnvironment<'a> {
    pub fn new(
        backend: &'a dyn ContainerBackend,
        approval: GuestApproval,
        fixtures: PathBuf,
    ) -> Result<Self> {
        approval.verify_here()?;
        let fixture_hashes = fixtures::hashes(&fixtures)?;
        Ok(Self {
            backend,
            approval,
            fixtures,
            fixture_hashes,
            container: None,
            scratch: None,
            epoch: 0,
            revision: 0,
            submitted: BTreeSet::new(),
        })
    }
    fn id(&self) -> Result<&String> {
        self.container
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("environment is not prepared"))
    }
    fn scratch_bind(&self) -> Result<VolumeMount> {
        let scratch = self
            .scratch
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("scratch not prepared"))?;
        Ok(VolumeMount {
            host_path: scratch
                .path()
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("invalid scratch path"))?
                .into(),
            container_path: "/work".into(),
            read_only: false,
        })
    }
    async fn command(&self, argv: &[&str]) -> Result<crate::container::ExecResult> {
        let result = self
            .backend
            .exec(self.id()?, argv, Duration::from_secs(30))
            .await?;
        ensure!(
            result.stdout.len() + result.stderr.len() <= 65536,
            "bounded observation output exceeded"
        );
        Ok(result)
    }
    async fn checked(&self, argv: &[&str]) -> Result<String> {
        let result = self.command(argv).await?;
        evaluate_assertion(
            &Assertion {
                exit_code: Some(0),
                ..Default::default()
            },
            result.exit_code,
            &result.stdout,
            &result.stderr,
        )?;
        Ok(result.stdout)
    }
}
#[async_trait]
impl Environment for ConaryEnvironment<'_> {
    fn fixture_source(&self) -> Option<&std::path::Path> {
        Some(&self.fixtures)
    }
    async fn reset(&mut self) -> Result<Observation> {
        self.approval.verify_here()?;
        self.close().await?;
        ensure!(
            fixtures::hashes(&self.fixtures)? == self.fixture_hashes,
            "fixture bytes changed"
        );
        self.scratch = Some(
            tempfile::Builder::new()
                .prefix("episode-")
                .tempdir_in(super::sandbox::SCRATCH_MOUNT)?,
        );
        let config = ContainerConfig {
            image: self.approval.image.clone(),
            env: HashMap::new(),
            volumes: vec![self.scratch_bind()?],
            privileged: false,
            network_mode: "none".into(),
            tmpfs: HashMap::from([("/tmp".into(), "size=16m,mode=1777".into())]),
            memory_limit: Some(512 * 1024 * 1024),
            experiment: true,
        };
        self.container = Some(self.backend.create(config).await?);
        self.backend.start(self.id()?).await?;
        self.verify().await?;
        let binary = self.checked(&["sha256sum", "/usr/bin/conary"]).await?;
        ensure!(
            binary.split_whitespace().next() == Some(&self.approval.conary_sha256),
            "Conary binary digest mismatch"
        );
        self.checked(&["mkdir", "-p", "/work/fixtures", ROOT])
            .await?;
        for name in fixtures::MEMBERS {
            let data = std::fs::read(self.fixtures.join(name))?;
            self.backend
                .copy_to(self.id()?, &format!("/work/fixtures/{name}"), &data)
                .await?;
        }
        self.checked(&["/usr/bin/conary", "system", "init", "--db-path", DB])
            .await?;
        self.epoch += 1;
        self.revision = 0;
        self.submitted.clear();
        self.observe().await
    }
    async fn verify(&self) -> Result<()> {
        self.approval.verify_here()?;
        let inspect = self.backend.inspect_container(self.id()?).await?;
        let isolation = inspect
            .isolation
            .ok_or_else(|| anyhow::anyhow!("runtime lacks isolation evidence"))?;
        ensure!(
            &isolation.id == self.id()?
                && isolation.image == self.approval.image
                && isolation.running,
            "environment identity/running state mismatch"
        );
        ensure!(
            !isolation.privileged
                && isolation.host_mounts == 1
                && isolation.read_only
                && isolation.binds == vec![self.scratch_bind()?],
            "experiment exposes host mounts or writable image/privileges"
        );
        ensure!(
            inspect.network_mode.as_deref() == Some("none")
                && inspect.memory_limit == Some(512 * 1024 * 1024)
                && isolation.cpu_nanos == 2_000_000_000
                && isolation.pids_limit == 128,
            "resource/network guard failed"
        );
        ensure!(
            inspect.tmpfs.get("/tmp").is_some_and(|v| v
                .split(',')
                .any(|p| p == "size=16m" || p == "size=16777216")),
            "temporary storage limit missing"
        );
        let process = self.checked(&["cat", "/proc/self/status"]).await?;
        super::sandbox::verify_process_restrictions(&process)?;
        Ok(())
    }
    async fn observe(&mut self) -> Result<Observation> {
        let scratch = self
            .scratch
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("scratch not prepared"))?;
        let facts = super::selected_state::observe(&scratch.path().join("root/var/lib/conary"))?;
        Ok(Observation {
            version: VERSION,
            environment: self.id()?.clone(),
            epoch: self.epoch,
            revision: self.revision,
            facts,
        })
    }
    async fn execute(&mut self, operation_id: &str, action: &Action) -> Result<Receipt> {
        self.verify().await?;
        ensure!(
            self.submitted.insert(operation_id.into()),
            "duplicate operation refused"
        );
        let result = match action {
            Action::Install(fixture) | Action::Update(fixture) => {
                self.command(&[
                    "/usr/bin/conary",
                    "ccs",
                    "install",
                    &format!("/work/fixtures/{}", fixture.filename()),
                    "--policy",
                    "/work/fixtures/policy.toml",
                    "--db-path",
                    DB,
                    "--root",
                    ROOT,
                    "--yes",
                ])
                .await?
            }
            Action::Remove(package) => {
                self.command(&[
                    "/usr/bin/conary",
                    "remove",
                    package.name(),
                    "--db-path",
                    DB,
                    "--root",
                    ROOT,
                    "--yes",
                ])
                .await?
            }
            Action::Inspect | Action::Check | Action::NegativeControl => {
                crate::container::ExecResult {
                    exit_code: 0,
                    stdout: "independent observation follows".into(),
                    stderr: String::new(),
                }
            }
            Action::Stop => anyhow::bail!("stop cannot dispatch"),
        };
        self.revision += 1;
        Ok(Receipt {
            operation_id: operation_id.into(),
            action: action.clone(),
            exit_code: result.exit_code,
            stdout: result.stdout,
            stderr: result.stderr,
        })
    }
    async fn close(&mut self) -> Result<()> {
        if let Some(id) = &self.container {
            self.backend.remove(id).await?;
        }
        self.container = None;
        if let Some(scratch) = self.scratch.take() {
            scratch.close()?;
        }
        Ok(())
    }
}
