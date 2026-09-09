// apps/conary/src/commands/update/failure.rs
//! Retain each requested update's error and earlier committed changesets.

#[derive(Debug)]
pub(crate) struct UpdatePackageFailure {
    pub package: String,
    pub version: String,
    pub error: anyhow::Error,
}

#[derive(Debug, thiserror::Error)]
#[error("{} of {total_requested} requested package update(s) failed: {}", failures.len(), self.details())]
pub(crate) struct UpdateFailures {
    pub failures: Vec<UpdatePackageFailure>,
    pub total_requested: usize,
    pub committed_changesets: Vec<i64>,
}

impl UpdateFailures {
    fn details(&self) -> String {
        self.failures
            .iter()
            .map(|failure| {
                format!(
                    "{} {} ({})",
                    failure.package,
                    failure.version,
                    crate::commands::package_failure::plain_failure_summary(&failure.error),
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }
}
