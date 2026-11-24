//! Inspecting a failed gate check run for a validly signed `result: "error"`
//! attestation (`docs/ci-attestation.md`, "Predicate"). Never counts as a
//! passing gate.

use serde_json::Value;
use sha2::{Digest, Sha256};

use conary_core::op::Repo;

use super::{
    ARTIFACT, AUDIENCE, Artifact, Key, Predicate, Signer, Trust, check_run_id, commit_statement,
    hex, hosted, is_commit, issued, placed, service, unzip, verified,
};
use crate::api::{ApiError, CheckRun, Source};
use crate::train::Forge;

/// Whether a failed gate check run carries a validly signed `result: "error"`
/// attestation from `trust` for `head` (matching `docs/ci-attestation.md`'s
/// gate `error` result): returns `Ok(Some(why))` when the gate itself broke
/// without judging the PR, `Ok(None)` otherwise (including attested `fail` or
/// unattested failures). Never counts as a passing gate. Only GitHub not
/// answering is an error.
pub fn gate_error(
    forge: &dyn Forge,
    source: &dyn Source,
    repo: &Repo,
    head: &str,
    run: &CheckRun,
    trust: &Trust,
) -> Result<Option<String>, ApiError> {
    if run.status != "completed"
        || run.conclusion.as_deref() != Some("failure")
        || service::carried(run).is_some()
    {
        return Ok(None);
    }
    let (signer, fresh) = (&trust.signer, trust.fresh.as_deref());
    let full = repo.to_string();
    let Some(job) = forge.job(&full, run.id)? else {
        return Ok(None);
    };
    let kept: Vec<Artifact> = forge
        .artifacts(&full, job.run_id)?
        .into_iter()
        .filter(|a| a.name.starts_with(ARTIFACT) && !a.expired)
        .collect();
    if kept.is_empty() {
        return Ok(None);
    }
    let keys = forge.keys()?;
    for a in kept {
        let Ok(file) = unzip(&forge.artifact(&full, a.id)?) else {
            continue;
        };
        let Ok((why, workflow_sha, is_hosted)) =
            check_gate_error(&file, &keys, repo, head, run, signer)
        else {
            continue;
        };
        if placed(
            forge,
            source,
            signer,
            fresh,
            is_hosted,
            job.runner_name.as_deref(),
            &workflow_sha,
        )?
        .is_none()
        {
            return Ok(Some(why));
        }
    }
    Ok(None)
}

fn check_gate_error(
    file: &[u8],
    keys: &[Key],
    repo: &Repo,
    head: &str,
    run: &CheckRun,
    signer: &Signer,
) -> Result<(String, String, bool), String> {
    let file = std::str::from_utf8(file).map_err(|_| "it isn't UTF-8")?;
    let (statement, token) = file
        .strip_suffix('\n')
        .and_then(|f| f.rsplit_once('\n'))
        .ok_or("it isn't a statement and then a token")?;
    let statement = format!("{statement}\n");
    let claims = verified(token, keys)?;
    let said = |name: &str| claims.get(name).and_then(Value::as_str).unwrap_or_default();
    let audience = format!("{AUDIENCE}{}", hex(&Sha256::digest(&statement)));
    let workflow_sha = issued(&claims, &audience, signer)?;
    if !said("repository").eq_ignore_ascii_case(&repo.to_string())
        || check_run_id(&claims) != Some(run.id)
    {
        return Err("token binding mismatch".into());
    }
    let predicate = if statement.starts_with("conary commit v1\n") {
        if said("sha") != head {
            return Err("commit sha mismatch".into());
        }
        let (subject, predicate) = commit_statement(&statement)?;
        if subject != head || predicate.revision.as_ref().is_some_and(|r| !r.is_null()) {
            return Err("commit statement mismatch".into());
        }
        predicate
    } else {
        let mut lines = statement.lines();
        let (Some("conary revision v1"), Some(base), Some(of), Some(pred), None) = (
            lines.next(),
            lines.next(),
            lines.next(),
            lines.next(),
            lines.next(),
        ) else {
            return Err("not a gate statement".into());
        };
        if of != head || !is_commit(base) {
            return Err("revision subject mismatch".into());
        }
        let predicate: Predicate = serde_json::from_str(pred).map_err(|e| e.to_string())?;
        if predicate.revision != Some(serde_json::json!({"base": base, "head": head})) {
            return Err("revision predicate mismatch".into());
        }
        predicate
    };
    if predicate.result != "error" {
        return Err("not a gate error".into());
    }
    let why = predicate
        .error
        .filter(|w| !w.trim().is_empty() && !w.contains(['\n', '\r']))
        .ok_or("missing error reason")?;
    Ok((why, workflow_sha.to_owned(), hosted(&claims)))
}
