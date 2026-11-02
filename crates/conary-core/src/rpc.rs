//! The ledger's calls (`docs/DESIGN.md` section 7), as conaryd serves them
//! over its socket and the MCP server offers them as tools: JSON-RPC 2.0,
//! one request per line and one response per line. Each call is in
//! [`METHODS`], with the JSON Schema of its params, so every transport
//! offers the same ones.
//!
//! Every call reads the ledger and nothing else. None runs git: a caller
//! names checkouts conaryd's user has no reason to trust, and git runs what
//! a checkout's config tells it to. `why` blames the line in the caller's
//! own process, and asks for the repository's `landings` here.

use std::ops::Deref;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::id::{ChangeId, RevisionId};
use crate::ledger::{Change, Ledger, Status};
use crate::notes::{self, Links};
use crate::op::Repo;

/// A call, as `tools/list` and the docs describe it.
pub struct Method {
    pub name: &'static str,
    pub description: &'static str,
    /// The JSON Schema of its params.
    pub params: fn() -> Value,
}

pub const METHODS: &[Method] = &[
    Method {
        name: "change.get",
        description: "One change: its intent, author and session, revisions, evidence and landing. By its id, or by repo and PR.",
        params: || {
            json!({
                "type": "object",
                "properties": {
                    "change": {"type": "string", "description": "The change's id."},
                    "repo": {"type": "string", "description": "owner/name"},
                    "pr": {"type": "integer", "minimum": 1},
                },
                "additionalProperties": false,
            })
        },
    },
    Method {
        name: "change.list",
        description: "Every change the ledger holds, oldest first, each in brief; the filters narrow it.",
        params: || {
            json!({
                "type": "object",
                "properties": {
                    "repo": {"type": "string", "description": "owner/name"},
                    "session": {"type": "string", "description": "The agent session that made it."},
                    "status": {"enum": ["open", "closed", "landed"]},
                    "since": {"type": "integer", "description": "First seen at or after this, in milliseconds since the Unix epoch."},
                },
                "additionalProperties": false,
            })
        },
    },
    Method {
        name: "evidence.query",
        description: "A change's evidence, recorded and never counted until attested: every check and review result, or one revision's.",
        params: || {
            json!({
                "type": "object",
                "properties": {
                    "change": {"type": "string", "description": "The change's id."},
                    "revision": {"type": "string", "description": "Only this revision's."},
                },
                "required": ["change"],
                "additionalProperties": false,
            })
        },
    },
    Method {
        name: "landings",
        description: "The note of every change landed in a repository, oldest first: its change, intent summary, author and session, landing and the landed revision's evidence. What why answers from.",
        params: || {
            json!({
                "type": "object",
                "properties": {
                    "repo": {"type": "string", "description": "owner/name"},
                },
                "required": ["repo"],
                "additionalProperties": false,
            })
        },
    },
];

/// A JSON-RPC error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub code: i64,
    pub message: String,
}

impl Error {
    pub const PARSE: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const NO_METHOD: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    /// The call was well formed, and failed: the log didn't verify, say.
    pub const FAILED: i64 = -32000;

    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn params(e: impl std::fmt::Display) -> Self {
        Self::new(Self::INVALID_PARAMS, e.to_string())
    }
}

fn params<T: DeserializeOwned>(params: &Value) -> Result<T, Error> {
    // Absent params are no params.
    let params = if params.is_null() { &json!({}) } else { params };
    serde_json::from_value(params.clone()).map_err(Error::params)
}

fn repo(text: &str) -> Result<Repo, Error> {
    text.parse().map_err(Error::params)
}

fn change<'a>(ledger: &'a Ledger, id: &str) -> Result<Option<&'a Change>, Error> {
    let id: ChangeId = id.parse().map_err(Error::params)?;
    Ok(ledger.change(id))
}

/// A change in full, with its URL and status.
pub fn full(c: &Change) -> Value {
    let mut v = serde_json::to_value(c).expect("a change serializes");
    v["url"] = json!(c.url());
    v["status"] = json!(c.status());
    v
}

/// A change in brief, as `change.list` and `cn log` give it.
pub fn summary(c: &Change) -> Value {
    json!({
        "change": c.id.to_string(),
        "repo": c.repo.to_string(),
        "pr": c.pr,
        "url": c.url(),
        "status": c.status(),
        "author": c.author,
        "summary": c.intent.as_ref().map(|i| &i.summary),
        "task": c.intent.as_ref().and_then(|i| i.task.as_ref()),
        "landed": c.landing.as_ref().map(|l| l.after.to_string()),
        "reverts": c.undo.as_ref().map(|u| &u.reverts),
        "reverted_by": c.reverted_by,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GetParams {
    change: Option<String>,
    repo: Option<String>,
    pr: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListParams {
    repo: Option<String>,
    session: Option<String>,
    status: Option<Status>,
    since: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceParams {
    change: String,
    revision: Option<RevisionId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LandingsParams {
    repo: String,
}

/// The result of calling `method` with `params` on `ledger`. A change that
/// isn't there is `null`, not an error.
pub fn call(ledger: &Ledger, method: &str, raw: &Value) -> Result<Value, Error> {
    match method {
        "change.get" => {
            let p: GetParams = params(raw)?;
            let found = match (p.change, p.repo, p.pr) {
                (Some(id), None, None) => change(ledger, &id)?,
                (None, Some(r), Some(pr)) => ledger.by_pr(&repo(&r)?, pr),
                _ => return Err(Error::params("give change, or repo and pr")),
            };
            Ok(found.map_or(Value::Null, full))
        }
        "change.list" => {
            let p: ListParams = params(raw)?;
            let on = p.repo.as_deref().map(repo).transpose()?;
            let listed = ledger
                .changes()
                .filter(|c| on.as_ref().is_none_or(|r| &c.repo == r))
                .filter(|c| {
                    p.session
                        .as_ref()
                        .is_none_or(|s| c.author.session.as_ref() == Some(s))
                })
                .filter(|c| p.status.is_none_or(|s| c.status() == s))
                .filter(|c| p.since.is_none_or(|t| c.seen >= t))
                .map(summary)
                .collect();
            Ok(Value::Array(listed))
        }
        "evidence.query" => {
            let p: EvidenceParams = params(raw)?;
            Ok(match change(ledger, &p.change)? {
                Some(c) => json!(
                    c.evidence
                        .iter()
                        .filter(|e| p.revision.is_none_or(|r| e.revision == r))
                        .collect::<Vec<_>>()
                ),
                None => Value::Null,
            })
        }
        "landings" => {
            let p: LandingsParams = params(raw)?;
            // The socket is on the ledger's own box, so nothing is left out.
            let on = repo(&p.repo)?;
            let landed: Vec<_> = notes::landed(ledger, &on, Links::Kept).collect();
            Ok(json!(landed))
        }
        _ => Err(Error::new(
            Error::NO_METHOD,
            format!("no method {method:?}"),
        )),
    }
}

/// A well-formed request, as [`parse`] reads it.
#[derive(Deserialize)]
pub struct Request {
    jsonrpc: String,
    /// Absent for a notification, which gets no response. A request's is a
    /// string, a number or null.
    #[serde(skip)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

fn error(id: Value, e: &Error) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": e.code, "message": e.message}}).to_string()
}

/// The response to a line refused before it was read as a request: one
/// too long to read, say.
pub fn refused(message: &str) -> String {
    error(Value::Null, &Error::new(Error::PARSE, message))
}

/// One request line read as a request, or the response that refuses it.
pub fn parse(line: &str) -> Result<Request, String> {
    let value: Value = serde_json::from_str(line).map_err(|e| refused(&e.to_string()))?;
    let invalid =
        |id: Value, message: &str| error(id, &Error::new(Error::INVALID_REQUEST, message));
    let Value::Object(fields) = &value else {
        return Err(invalid(Value::Null, "a request is an object"));
    };
    // A null id is a request's: only an absent one makes a notification.
    let id = fields.get("id").cloned();
    if id
        .as_ref()
        .is_some_and(|i| !(i.is_null() || i.is_string() || i.is_number()))
    {
        return Err(invalid(Value::Null, "an id is a string, a number or null"));
    }
    let shown = id.clone().unwrap_or(Value::Null);
    let mut request: Request =
        serde_json::from_value(value).map_err(|e| invalid(shown.clone(), &e.to_string()))?;
    if request.jsonrpc != "2.0" {
        return Err(invalid(shown, "jsonrpc must be \"2.0\""));
    }
    request.id = id;
    Ok(request)
}

/// The response to `request` that `result` gives, or `None` for a
/// notification.
pub fn respond(request: Request, result: Result<Value, Error>) -> Option<String> {
    let id = request.id?;
    Some(match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
        Err(e) => error(id, &e),
    })
}

/// The response to one request line, or `None` for a notification.
/// `ledger` is called only once the request is well formed, so a caller
/// can refresh the ledger in it.
pub fn answer<L: Deref<Target = Ledger>>(
    line: &str,
    ledger: impl FnOnce() -> Result<L, String>,
) -> Option<String> {
    let request = match parse(line) {
        Ok(r) => r,
        Err(refusal) => return Some(refusal),
    };
    let result = ledger()
        .map_err(|e| Error::new(Error::FAILED, e))
        .and_then(|l| call(&l, &request.method, &request.params));
    respond(request, result)
}

#[cfg(test)]
mod tests;
