//! A job's own token, on its own (`docs/ci-attestation.md`, "A job's own
//! token"): what a door that takes something from CI asks before it takes
//! it. The token is held to the rules evidence is, but for the statement:
//! its audience names what the job hands over, which the door checks.

use serde::Serialize;
use serde_json::{Map, Value};

use conary_core::op::Repo;

use super::{AUDIENCE, Signer, check_run_id, hosted, issued, placed, verified};
use crate::api::{ApiError, Source};
use crate::train::Forge;

/// How far a token's `exp` and `nbf` may be from the checker's clock and
/// still hold: no two clocks agree to the second.
pub const SKEW: i64 = 60;

/// A job whose token holds, and what its token says of the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Admitted {
    /// The repository whose run it was, as owner/name.
    pub repository: String,
    /// `public`, `private` or `internal`, as GitHub says.
    pub visibility: String,
    /// The event that started the run, such as `push` or `pull_request`.
    pub event: String,
    /// The ref and commit the run was for: for a PR, GitHub's merge.
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub sha: String,
    /// The signer workflow's commit, on its repo's default branch.
    pub workflow_sha: String,
    /// The runner that took the job, when it wasn't one of GitHub's own.
    pub runner: Option<String>,
}

/// What a door asks of a job's token, by the machine's trust: the
/// [`Trust`](super::Trust)'s signer and fresh runners, and its own audience
/// and repos.
#[derive(Debug, Clone, Copy)]
pub struct Door<'a> {
    pub audience: &'a str,
    pub repos: &'a [Repo],
    pub signer: &'a Signer,
    pub fresh: Option<&'a str>,
}

/// Whether `token`, a GitHub Actions job's own OIDC token, counts for
/// `audience` at `now` (seconds since the Unix epoch): GitHub signed it for
/// that audience alone, it hasn't expired, its run is one of `repos`' (rule
/// 2's "the token's repo is the change's": anyone's repo can call the
/// signer workflow), its job is `signer`'s at a commit
/// on its default branch, and it ran on a runner GitHub hosts or one named
/// as `fresh` names the machine's fresh ones. What it says, or why it
/// doesn't count. Only GitHub not answering is an error.
///
/// An attestation's audience is refused: its token counts only with its
/// statement, which a bare token isn't checked against.
pub fn admitted(
    forge: &dyn Forge,
    source: &dyn Source,
    token: &str,
    door: &Door,
    now: i64,
) -> Result<Result<Admitted, String>, ApiError> {
    let Door {
        audience,
        repos,
        signer,
        fresh,
    } = *door;
    if audience.starts_with(AUDIENCE) {
        return Ok(Err(format!(
            "{audience} is an attestation's audience, whose token counts only with its statement"
        )));
    }
    let claims = match verified(token, &forge.keys()?) {
        Ok(claims) => claims,
        Err(e) => return Ok(Err(e)),
    };
    let workflow_sha = match issued(&claims, audience, signer).and_then(|sha| {
        live(&claims, now)?;
        Ok(sha)
    }) {
        Ok(sha) => sha,
        Err(e) => return Ok(Err(e)),
    };
    let said = |name: &str| {
        claims
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let repository = said("repository");
    if !repos
        .iter()
        .any(|r| r.to_string().eq_ignore_ascii_case(&repository))
    {
        return Ok(Err(format!(
            "its token is {repository:?}'s, which isn't listed"
        )));
    }
    let hosted = hosted(&claims);
    // Only GitHub's record of the job says which runner took it.
    let runner = if hosted {
        None
    } else {
        let Some(job) = check_run_id(&claims) else {
            return Ok(Err("its token names no job".into()));
        };
        let Some(job) = forge.job(&repository, job)? else {
            return Ok(Err(format!("{repository} has no job {job}")));
        };
        job.runner_name
    };
    if let Some(not) = placed(
        forge,
        source,
        signer,
        fresh,
        hosted,
        runner.as_deref(),
        workflow_sha,
    )? {
        return Ok(Err(not));
    }
    Ok(Ok(Admitted {
        visibility: said("repository_visibility"),
        event: said("event_name"),
        git_ref: said("ref"),
        sha: said("sha"),
        workflow_sha: workflow_sha.to_owned(),
        repository,
        runner,
    }))
}

/// Whether a token is good at `now`: it has an `exp` that hasn't passed,
/// and no `nbf` still to come, each give or take [`SKEW`]. A bare token is
/// taken at the door, where an old one is a replayed one.
fn live(claims: &Map<String, Value>, now: i64) -> Result<(), String> {
    let at = |name: &str| claims.get(name).and_then(Value::as_i64);
    match at("exp") {
        Some(exp) if now <= exp + SKEW => {}
        Some(exp) => return Err(format!("its token expired at {exp}")),
        None => return Err("its token has no expiry".into()),
    }
    match at("nbf") {
        Some(nbf) if nbf > now + SKEW => Err(format!("its token isn't good until {nbf}")),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests;
