//! Gate reuse: whether CI already attested a pass of a gate on exactly a
//! tree, so a push of that tree under another commit needn't run the gate
//! again (`docs/DESIGN.md` section 4.3, rule 3). `docs/gate-reuse.md` says
//! what counts as a hit; it answers burrow's `burrow-gate-reuse/v1`
//! contract (burrow's `crates/burrow-flake-check/README.md`, "Reusing an
//! attested pass"). It only reads: an answer is never recorded.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use conary_core::ledger::Ledger;
use conary_core::op::Repo;

use crate::Api;
use crate::api::{ApiError, CheckRun, Source};
use crate::attest::{self, Signer, Subject, Trust, attested, gated};
use crate::train::Forge;

/// The contract both sides name, in the request and in the answer.
pub const CONTRACT: &str = "burrow-gate-reuse/v1";
/// A request's token's audience: this, then the request's SHA-256.
pub const AUDIENCE: &str = "burrow-gate-reuse:v1:sha256:";
/// A request is a few hundred bytes; one far past that isn't one.
pub const MAX_BODY: usize = 64 * 1024;
/// The most passed check runs one answer reads the attestations of. Past
/// it, the lookup can't tell, and the gate runs.
const MOST_RUNS: usize = 32;
/// How far a requester's clock may be from ours, in seconds.
const LEEWAY: i64 = 60;

/// What the machine configures the lookup with, its own and never a
/// repository's: whose evidence counts and for which repositories it
/// answers.
#[derive(Debug, Clone)]
pub struct Config {
    /// The repositories it answers for. Any other is refused.
    pub repos: Vec<Repo>,
    /// Whose attestations count: the train's ([`attested`]).
    pub trust: Trust,
    /// Where GitHub's web pages are, which a hit's run is named under:
    /// `https://github.com` on github.com.
    pub server: String,
}

/// What the lookup answers a request it read and whose requester it
/// trusts: a hit names the run whose attestation it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Hit {
        run: String,
    },
    /// No attested pass of this gate on this tree.
    Miss {
        why: String,
    },
    /// It couldn't tell: GitHub or the ledger didn't answer, or there was
    /// more to read than one answer reads.
    Unknown {
        why: String,
    },
}

/// The HTTP answer to a request: its status and its JSON body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    pub body: Value,
}

impl Reply {
    fn refused(status: u16, why: impl Into<String>) -> Self {
        Self {
            status,
            body: json!({"contract": CONTRACT, "error": why.into()}),
        }
    }
}

/// The gate a request or an attestation names: burrow's gate identity.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Gate {
    /// The whole identity, without its workflow's commit: what two gates
    /// match on, the workflow's tree standing for its commit.
    key: Value,
    workflow: Workflow,
    /// Its `source` is `tree`: its verdict is the tree's alone, whichever
    /// commit holds it.
    by_tree: bool,
    /// The checks it evaluates but doesn't build (`inputs.build-skip`).
    skips: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Workflow {
    repository: String,
    file: String,
    commit: String,
    tree: String,
}

impl Gate {
    fn read(v: &Value) -> Result<Self, String> {
        let mut key = v.as_object().ok_or("the gate isn't an object")?.clone();
        let mut workflow = match key.get("workflow") {
            Some(Value::Object(w)) => w.clone(),
            _ => return Err("the gate names no workflow".into()),
        };
        let field = |w: &Map<String, Value>, name: &str| -> Result<String, String> {
            w.get(name)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("the gate's workflow has no {name}"))
        };
        let read = Workflow {
            repository: field(&workflow, "repository")?,
            file: field(&workflow, "file")?,
            commit: field(&workflow, "commit")?,
            tree: field(&workflow, "tree")?,
        };
        if !attest::is_commit(&read.commit) || !attest::is_commit(&read.tree) {
            return Err("the gate's workflow commit or tree isn't an object id".into());
        }
        workflow.remove("commit");
        key.insert("workflow".into(), Value::Object(workflow));
        let skips = match key.get("inputs").and_then(|i| i.get("build-skip")) {
            None => BTreeSet::new(),
            Some(Value::Array(a)) => a
                .iter()
                .map(|s| s.as_str().map(str::to_owned))
                .collect::<Option<_>>()
                .ok_or("the gate's build-skip isn't a list of names")?,
            Some(_) => return Err("the gate's build-skip isn't a list".into()),
        };
        Ok(Self {
            by_tree: key.get("source") == Some(&json!("tree")),
            key: Value::Object(key),
            workflow: read,
            skips,
        })
    }

    /// Whether its workflow is `signer`, the one the machine trusts.
    fn signed_as(&self, signer: &Signer) -> bool {
        self.workflow.file == signer.workflow
            && self
                .workflow
                .repository
                .eq_ignore_ascii_case(&signer.repo.to_string())
    }
}

/// A request, as burrow's runner sends it: one JSON object, keys sorted.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    contract: String,
    repository: String,
    commit: String,
    tree: String,
    gate: Value,
}

/// A request that reads.
#[derive(Debug)]
struct Request {
    /// Its SHA-256: the query an answer names, and its token's audience.
    query: String,
    repo: Repo,
    repository: String,
    commit: String,
    tree: String,
    gate: Gate,
}

impl Request {
    fn read(body: &[u8]) -> Result<Self, String> {
        if body.len() > MAX_BODY {
            return Err(format!("a request over {MAX_BODY} bytes"));
        }
        let wire: Wire =
            serde_json::from_slice(body).map_err(|e| format!("the request doesn't read: {e}"))?;
        if wire.contract != CONTRACT {
            return Err(format!("the request is under {:?}", wire.contract));
        }
        if !attest::is_commit(&wire.commit) || !attest::is_commit(&wire.tree) {
            return Err("the request's commit or tree isn't an object id".into());
        }
        Ok(Self {
            query: attest::hex(&Sha256::digest(body)),
            repo: wire
                .repository
                .parse()
                .map_err(|_| "the request's repository isn't owner/name")?,
            repository: wire.repository,
            commit: wire.commit,
            tree: wire.tree,
            gate: Gate::read(&wire.gate)?,
        })
    }
}

/// The answer to `body`, a request carrying `authorization` (its
/// `Authorization` header), at `now` (seconds since the Unix epoch). The
/// requester is the job asking: its GitHub OIDC token, issued for this
/// request, must be its repository's, for its commit, from the workflow its
/// gate names at the commit it names. `ledger` holds the trains' candidates,
/// or says why it can't be read. Only a request that doesn't read, or a
/// requester that isn't trusted, gets anything but 200.
pub fn answer(
    forge: &dyn Forge,
    source: &dyn Source,
    ledger: Result<&Ledger, String>,
    config: &Config,
    now: i64,
    authorization: Option<&str>,
    body: &[u8],
) -> Reply {
    let request = match Request::read(body) {
        Ok(r) => r,
        Err(why) => return Reply::refused(400, why),
    };
    if !config.repos.contains(&request.repo) {
        return Reply::refused(403, "the lookup doesn't answer for that repository");
    }
    let token = authorization
        .and_then(|a| a.strip_prefix("Bearer "))
        .map(str::trim);
    let Some(token) = token else {
        return Reply::refused(401, "no bearer token");
    };
    match requester(forge, &request, token, now) {
        Ok(Ok(())) => {}
        Ok(Err(why)) => return Reply::refused(401, why),
        Err(e) => return Reply::refused(503, format!("GitHub's keys can't be read: {e}")),
    }
    let answer = lookup(forge, source, ledger, config, &request);
    let mut body = json!({"contract": CONTRACT, "query": request.query});
    let (evidence, extra) = match answer {
        Answer::Hit { run } => ("hit", ("run", run)),
        Answer::Miss { why } => ("miss", ("why", why)),
        Answer::Unknown { why } => ("unknown", ("why", why)),
    };
    body["evidence"] = json!(evidence);
    body[extra.0] = json!(extra.1);
    Reply { status: 200, body }
}

/// Whether `token` is the requesting job's own, for this request: GitHub
/// signed it, for this request's digest, in its repository, at its commit,
/// in the workflow its gate names at that workflow's commit, and it hasn't
/// expired. Only GitHub not answering is an error.
fn requester(
    forge: &dyn Forge,
    request: &Request,
    token: &str,
    now: i64,
) -> Result<Result<(), String>, ApiError> {
    let keys = forge.keys()?;
    let claims = match attest::verified(token, &keys) {
        Ok(c) => c,
        Err(why) => return Ok(Err(why)),
    };
    let said = |name: &str| claims.get(name).and_then(Value::as_str).unwrap_or_default();
    let number = |name: &str| claims.get(name).and_then(Value::as_i64);
    let audience = format!("{AUDIENCE}{}", request.query);
    let audiences = match claims.get("aud") {
        Some(Value::String(a)) => vec![a.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    let w = &request.gate.workflow;
    let asked_by = format!("{}/{}@", w.repository, w.file);
    let checks = [
        (
            said("iss") == attest::ISSUER,
            "its issuer isn't GitHub Actions'",
        ),
        (
            audiences == [audience.as_str()],
            "its audience isn't this request's digest",
        ),
        (
            said("repository").eq_ignore_ascii_case(&request.repository),
            "it's another repository's",
        ),
        (said("sha") == request.commit, "it's for another commit"),
        (
            said("job_workflow_ref")
                .get(..asked_by.len())
                .is_some_and(|r| r.eq_ignore_ascii_case(&asked_by)),
            "it's another workflow's than the gate names",
        ),
        (
            said("job_workflow_sha") == w.commit,
            "its workflow is at another commit than the gate names",
        ),
        (
            number("exp").is_some_and(|exp| exp + LEEWAY > now),
            "it has expired",
        ),
        (
            number("nbf").is_none_or(|nbf| nbf - LEEWAY <= now),
            "it isn't valid yet",
        ),
    ];
    Ok(match checks.iter().find(|(ok, _)| !ok) {
        Some((_, why)) => Err(format!("the request's token: {why}")),
        None => Ok(()),
    })
}

/// Looks for the pass, once the requester is trusted.
fn lookup(
    forge: &dyn Forge,
    source: &dyn Source,
    ledger: Result<&Ledger, String>,
    config: &Config,
    request: &Request,
) -> Answer {
    let unknown = |why: String| Answer::Unknown { why };
    if !request.gate.by_tree {
        return Answer::Miss {
            why: "the gate's verdict isn't its tree's alone (its source isn't \"tree\")".into(),
        };
    }
    let repo = &request.repository;
    let w = &request.gate.workflow;
    // What the push holds, and the gate's own files, as GitHub has them:
    // equal trees are the whole match, so both are read, never taken.
    match forge.tree(repo, &request.commit) {
        Ok(t) if t == request.tree => {}
        Ok(t) => {
            return unknown(format!(
                "{} holds tree {t}, not {}",
                request.commit, request.tree
            ));
        }
        Err(e) => return unknown(format!("the push's tree can't be read: {e}")),
    }
    match forge.tree(&w.repository, &w.commit) {
        Ok(t) if t == w.tree => {}
        Ok(t) => {
            return unknown(format!(
                "the gate's workflow commit holds tree {t}, not {}",
                w.tree
            ));
        }
        Err(e) => return unknown(format!("the gate's workflow tree can't be read: {e}")),
    }
    let ledger = match ledger {
        Ok(l) => l,
        Err(why) => return unknown(format!("the ledger can't be read: {why}")),
    };
    let commits = match commits(source, ledger, request) {
        Ok(c) => c,
        Err(e) => return unknown(format!("GitHub didn't answer: {e}")),
    };
    let mut why = Vec::new();
    let mut unavailable = None;
    let mut read = 0;
    for commit in &commits {
        let runs = match source.check_runs(repo, commit) {
            Ok(runs) => runs,
            Err(e) => {
                unavailable = Some(e.to_string());
                continue;
            }
        };
        let mut passed: Vec<CheckRun> = runs
            .into_iter()
            .filter(|r| r.status == "completed" && r.conclusion.as_deref() == Some("success"))
            .collect();
        passed.sort_by_key(|r| std::cmp::Reverse(r.id));
        for run in passed {
            if read == MOST_RUNS {
                return unknown(format!(
                    "more than {MOST_RUNS} passed check runs to read; none read was a hit"
                ));
            }
            read += 1;
            match pass(forge, source, config, request, commit, &run) {
                Ok(Ok(run)) => return Answer::Hit { run },
                Ok(Err(e)) => why.push(format!("{} on {commit}: {e}", run.name)),
                Err(e) => unavailable = Some(e.to_string()),
            }
        }
    }
    if let Some(e) = unavailable {
        return unknown(format!("GitHub didn't answer for all of it: {e}"));
    }
    Answer::Miss {
        why: if why.is_empty() {
            format!(
                "no passed check run on {} commits of this tree",
                commits.len()
            )
        } else {
            why.join("; ")
        },
    }
}

/// The commits whose check runs could hold a pass on the request's tree:
/// the head of each PR GitHub says made the push (its gate ran on its merge
/// with trunk), each candidate of the repository's train that the ledger
/// says made that tree (its gate ran on it), and the push itself (an
/// earlier attempt of its own run). Newest candidates first; no commit
/// twice.
fn commits(
    source: &dyn Source,
    ledger: &Ledger,
    request: &Request,
) -> Result<Vec<String>, ApiError> {
    let mut commits: Vec<String> = Vec::new();
    for pr in source.commit_pulls(&request.repository, &request.commit)? {
        commits.push(source.pull(&request.repository, pr)?.head.sha);
    }
    if let Some(train) = ledger.train(&request.repo) {
        commits.extend(
            train
                .candidates()
                .iter()
                .rev()
                .filter_map(|id| ledger.candidate(*id))
                .filter(|c| c.tree.as_str() == request.tree)
                .map(|c| c.commit.as_str().to_owned()),
        );
    }
    commits.push(request.commit.clone());
    let mut seen = BTreeSet::new();
    commits.retain(|c| seen.insert(c.clone()));
    Ok(commits)
}

/// Whether `run`, a passed check run on `commit`, is the request's gate
/// passing on its tree: the run's own CI attestation, checked as the train
/// checks one, naming the same gate. Its run, if it is; why not, if not.
/// Only GitHub not answering is an error.
fn pass(
    forge: &dyn Forge,
    source: &dyn Source,
    config: &Config,
    request: &Request,
    commit: &str,
    run: &CheckRun,
) -> Result<Result<String, String>, ApiError> {
    let subject = Subject::Gate {
        commit,
        tree: &request.tree,
    };
    let attested = match attested(forge, source, &request.repo, subject, run, &config.trust)? {
        Ok(a) => a,
        Err(why) => return Ok(Err(why)),
    };
    let gated = match gated(&attested.attest) {
        Ok(g) => g,
        Err(why) => return Ok(Err(why)),
    };
    let Some(gate) = gated.gate else {
        return Ok(Err("its attestation names no gate".into()));
    };
    let gate = match Gate::read(&gate) {
        Ok(g) => g,
        Err(why) => return Ok(Err(format!("its attestation's gate: {why}"))),
    };
    let w = &gate.workflow;
    if !gate.signed_as(&config.trust.signer) || w.commit != gated.workflow_sha {
        return Ok(Err(format!(
            "its gate names {}/{} at {}, and its token {} at {}",
            w.repository, w.file, w.commit, config.trust.signer, gated.workflow_sha
        )));
    }
    if gate.key != request.gate.key {
        return Ok(Err(
            "it ran another gate: its workflow's files or its inputs differ".into(),
        ));
    }
    if forge.tree(&w.repository, &w.commit)? != w.tree {
        return Ok(Err(format!(
            "its gate says its workflow commit {} holds tree {}, and it doesn't",
            w.commit, w.tree
        )));
    }
    let unskipped: Vec<&String> = gated
        .skipped
        .iter()
        .filter(|s| !request.gate.skips.contains(*s))
        .collect();
    if !unskipped.is_empty() {
        return Ok(Err(format!(
            "it skipped {unskipped:?}, which this gate builds"
        )));
    }
    let Some(job) = forge.job(&request.repository, run.id)? else {
        return Ok(Err("it isn't a GitHub Actions job".into()));
    };
    Ok(Ok(format!(
        "{}/{}/actions/runs/{}/job/{}",
        config.server.trim_end_matches('/'),
        request.repository,
        job.run_id,
        run.id
    )))
}

/// The machine's configuration file for the lookup, as conaryd reads it
/// (`docs/gate-reuse.md`). A running daemon reads it again at each lookup
/// ([`Lookup`]), so every option but `listen` takes effect at the next one.
/// None of them starts paid work: a hit saves a gate's run, and anything
/// else runs the gate as it would without the lookup.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Where it listens for requests, behind TLS termination.
    #[schemars(extend(
        "x-spender" = false,
        "x-live" = false,
        "x-needs" = "a restart of conaryd: it binds the address once, as it starts"
    ))]
    pub listen: std::net::SocketAddr,
    /// The repositories it answers for, as `OWNER/REPO`. Any other is
    /// refused.
    #[serde(with = "named::each")]
    #[schemars(with = "Vec<String>", extend("x-spender" = false))]
    pub repos: Vec<Repo>,
    /// The program that prints a repository's GitHub token, as `cn`'s, run
    /// without a shell. Its arguments may carry a credential, so its value is
    /// never printed.
    #[schemars(extend("x-spender" = false, "x-secret" = true))]
    pub token_command: String,
    /// The signer workflow whose attestations count, as the train's,
    /// `OWNER/REPO/.github/workflows/FILE`.
    #[serde(with = "named")]
    #[schemars(with = "String", extend("x-spender" = false))]
    pub signer: Signer,
    /// What the names of self-hosted runners started fresh for one job start
    /// with, as the train's; `null` counts GitHub's hosted runners alone.
    #[serde(default)]
    #[schemars(extend("x-spender" = false))]
    pub fresh: Option<String>,
    /// The GitHub API root.
    #[serde(default = "api")]
    #[schemars(extend("x-spender" = false))]
    pub api: String,
    /// GitHub's web root, which a hit's run is named under.
    #[serde(default = "server")]
    #[schemars(extend("x-spender" = false))]
    pub server: String,
}

fn api() -> String {
    "https://api.github.com".into()
}

fn server() -> String {
    "https://github.com".into()
}

/// `signer` and `repos` as the file writes them, as strings, read into what
/// they name, so the file's one loader refuses one that doesn't parse. The
/// refusal never quotes it: a value there may be a credential written in
/// the wrong place, and the types' own errors quote what they're given.
mod named {
    use serde::de::{Deserializer, Error};
    use serde::{Deserialize, Serializer};
    use std::fmt::Display;
    use std::str::FromStr;

    pub fn serialize<T: Display, S: Serializer>(value: &T, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(value)
    }
    pub fn deserialize<'de, T: FromStr, D: Deserializer<'de>>(d: D) -> Result<T, D::Error> {
        parse(&String::deserialize(d)?)
    }
    fn parse<T: FromStr, E: Error>(text: &str) -> Result<T, E> {
        text.parse()
            .map_err(|_| E::custom("a value doesn't parse as what its option names"))
    }

    /// A list of them.
    pub mod each {
        use super::*;

        pub fn serialize<T: Display, S: Serializer>(values: &[T], s: S) -> Result<S::Ok, S::Error> {
            s.collect_seq(values.iter().map(ToString::to_string))
        }
        pub fn deserialize<'de, T: FromStr, D: Deserializer<'de>>(
            d: D,
        ) -> Result<Vec<T>, D::Error> {
            Vec::<String>::deserialize(d)?
                .iter()
                .map(|text| parse(text))
                .collect()
        }
    }
}

impl Settings {
    /// The lookup's configuration. Services' producers aren't trusted for
    /// a gate's tree: only CI's own attestation says it.
    pub fn config(&self) -> Config {
        Config {
            repos: self.repos.clone(),
            trust: Trust {
                signer: self.signer.clone(),
                fresh: self.fresh.clone(),
                producers: attest::service::Producers::default(),
            },
            server: self.server.clone(),
        }
    }
}

/// The lookup as conaryd runs it: its configuration file read again at
/// each request, but for `listen`, which keeps the address it bound.
pub struct Lookup {
    settings: crate::flow::schema::Live<Settings>,
    /// The API the last request read GitHub through, kept while its root
    /// and token command stay the same, so its tokens and GitHub's keys are
    /// read once rather than at each request.
    api: Mutex<Option<(String, String, Arc<Api>)>>,
}

impl Lookup {
    /// The lookup configured by the file at `path`, read as it is now. A
    /// file that doesn't read, or whose options don't, is refused here, so a
    /// daemon never starts on one.
    pub fn open(path: &Path) -> Result<Self, String> {
        let settings =
            crate::flow::schema::Live::<Settings>::open(path).map_err(|e| e.to_string())?;
        let lookup = Self {
            settings,
            api: Mutex::default(),
        };
        lookup.configured(lookup.settings.started().clone())?;
        Ok(lookup)
    }

    /// The configuration conaryd started with: its `listen` is the one it
    /// binds.
    pub fn started(&self) -> &Settings {
        self.settings.started()
    }

    /// The answer to a request, as [`answer`] gives it, under the
    /// configuration as it reads now. When that doesn't read, nothing is
    /// trusted, so the request is refused with 503 and the gate runs; the
    /// reason goes to conaryd's log, never to the requester.
    pub fn answer(
        &self,
        ledger: Result<&Ledger, String>,
        now: i64,
        authorization: Option<&str>,
        body: &[u8],
    ) -> Reply {
        let read = self
            .settings
            .read()
            .map_err(|e| e.to_string())
            .and_then(|settings| self.configured(settings));
        match read {
            Ok((config, api)) => answer(&*api, &*api, ledger, &config, now, authorization, body),
            Err(e) => {
                eprintln!(
                    "conaryd: the gate-reuse configuration doesn't read, so lookups are refused: {e}"
                );
                Reply::refused(503, "the lookup's configuration doesn't read")
            }
        }
    }

    /// `settings`' configuration, and the API to read GitHub through: the
    /// last one, unless its root or token command changed.
    fn configured(&self, settings: Settings) -> Result<(Config, Arc<Api>), String> {
        let config = settings.config();
        let mut kept = self.api.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((root, command, api)) = &*kept
            && *root == settings.api
            && *command == settings.token_command
        {
            return Ok((config, Arc::clone(api)));
        }
        let tokens = crate::Tokens::command(&settings.token_command).map_err(|e| e.to_string())?;
        let api = Arc::new(Api::new(&settings.api, tokens));
        *kept = Some((settings.api, settings.token_command, Arc::clone(&api)));
        Ok((config, api))
    }
}

#[cfg(test)]
mod tests;
