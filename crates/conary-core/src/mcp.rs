//! The MCP server's side of MCP (`docs/mcp.md`): the ledger's calls
//! ([`rpc::METHODS`]) as tools, each forwarded to conaryd, which answers
//! it, and `why`, which blames the line here, in the agent's own process,
//! and asks conaryd for the repository's landings. Over stdio, a line a
//! message, as JSON-RPC 2.0 like conaryd itself.

use std::path::Path;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::notes::Note;
use crate::rpc::{self, Error, METHODS};
use crate::why::{self, Source};

/// The MCP versions this server speaks, newest first. A client asking for
/// another gets the newest, and decides for itself. None older than
/// 2025-06-18: those take JSON-RPC batches, and a line here is one message.
pub const VERSIONS: &[&str] = &["2025-11-25", "2025-06-18"];

const INSTRUCTIONS: &str = "Conary's ledger: every PR on this machine's repos, \
with its intent, author and session, revisions, evidence and landing. Use why \
to find the change that brought a line onto trunk, and change_get for all of \
a change.";

/// A call's tool name: its method's, with `_` for `.`, which tool names
/// can't hold everywhere.
pub fn tool(method: &str) -> String {
    method.replace('.', "_")
}

/// The one tool answered here, not by conaryd: git runs on the agent's
/// checkout as the agent, never as conaryd.
const WHY: &str = "why";

fn why_params() -> Value {
    json!({
        "type": "object",
        "properties": {
            "dir": {"type": "string", "description": "The checkout, as an absolute path."},
            "path": {"type": "string", "description": "The file, relative to the checkout."},
            "line": {"type": "integer", "minimum": 1},
        },
        "required": ["dir", "path", "line"],
        "additionalProperties": false,
    })
}

fn tools() -> Value {
    let describe = |name: String, description: &str, params: Value| {
        json!({
            "name": name,
            "description": description,
            "inputSchema": params,
            "annotations": {"readOnlyHint": true, "openWorldHint": false},
        })
    };
    let mut tools: Vec<Value> = METHODS
        .iter()
        .map(|m| describe(tool(m.name), m.description, (m.params)()))
        .collect();
    tools.push(describe(
        WHY.into(),
        "Why a line is the way it is: the change whose landing brought it onto trunk, its intent, session and evidence. In a checkout on the ledger's machine.",
        why_params(),
    ));
    json!({"tools": tools})
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WhyParams {
    dir: String,
    path: String,
    line: u32,
}

/// `why`: the blame here, and the landings from conaryd.
fn why(
    arguments: Map<String, Value>,
    forward: impl FnOnce(&str, &Value) -> Result<Value, String>,
) -> Result<Value, String> {
    let p: WhyParams =
        serde_json::from_value(Value::Object(arguments)).map_err(|e| e.to_string())?;
    let dir = Path::new(&p.dir);
    if !dir.is_absolute() {
        return Err("dir must be an absolute path".into());
    }
    let blamed = why::blame(dir, Path::new(&p.path), p.line).map_err(|e| e.to_string())?;
    let landings = forward("landings", &json!({"repo": blamed.repo.to_string()}))?;
    let notes: Vec<Note> =
        serde_json::from_value(landings).map_err(|e| format!("conaryd's landings: {e}"))?;
    Ok(json!(blamed.why(Source::Ledger, notes)))
}

#[derive(Deserialize)]
struct Initialize {
    #[serde(rename = "protocolVersion")]
    version: String,
}

#[derive(Deserialize)]
struct Call {
    name: String,
    #[serde(default)]
    arguments: Map<String, Value>,
}

fn params<T: serde::de::DeserializeOwned>(params: &Value) -> Result<T, Error> {
    serde_json::from_value(params.clone())
        .map_err(|e| Error::new(Error::INVALID_PARAMS, e.to_string()))
}

/// The response to one line from the client, or `None` for a
/// notification. `forward` makes a call of conaryd's: its result, or what
/// went wrong, which the agent reads as the tool's error.
pub fn answer(
    line: &str,
    forward: impl FnOnce(&str, &Value) -> Result<Value, String>,
) -> Option<String> {
    let request = match rpc::parse(line) {
        Ok(r) => r,
        Err(refusal) => return Some(refusal),
    };
    let result = match request.method.as_str() {
        "initialize" => params::<Initialize>(&request.params).map(|p| {
            let version = VERSIONS
                .iter()
                .find(|v| **v == p.version)
                .unwrap_or(&VERSIONS[0]);
            json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "conary", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS,
            })
        }),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools()),
        "tools/call" => params::<Call>(&request.params).and_then(|p| {
            let answered = if p.name == WHY {
                why(p.arguments, forward)
            } else {
                let method = METHODS
                    .iter()
                    .find(|m| tool(m.name) == p.name)
                    .ok_or_else(|| {
                        Error::new(Error::INVALID_PARAMS, format!("no tool {:?}", p.name))
                    })?;
                forward(method.name, &Value::Object(p.arguments))
            };
            let (text, failed) = match answered {
                Ok(v) => (v.to_string(), false),
                Err(e) => (e, true),
            };
            Ok(json!({"content": [{"type": "text", "text": text}], "isError": failed}))
        }),
        // Notifications (initialized, cancelled) need nothing from us.
        _ if request.id.is_none() => return None,
        other => Err(Error::new(Error::NO_METHOD, format!("no method {other:?}"))),
    };
    rpc::respond(request, result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(line: &str, forward: impl FnOnce(&str, &Value) -> Result<Value, String>) -> Value {
        serde_json::from_str(&answer(line, forward).unwrap()).unwrap()
    }

    fn never(method: &str, _: &Value) -> Result<Value, String> {
        panic!("forwarded {method}")
    }

    #[test]
    fn initialize_agrees_a_version() {
        let line = |v: &str| {
            json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
                "params": {"protocolVersion": v, "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}})
            .to_string()
        };
        let r = reply(&line("2025-06-18"), never);
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(r["result"]["serverInfo"]["name"], "conary");
        assert!(r["result"]["capabilities"]["tools"].is_object());
        // Older versions take batches, which a line here never is.
        for older in ["2025-03-26", "2024-11-05", "1999-01-01"] {
            let r = reply(&line(older), never);
            assert_eq!(r["result"]["protocolVersion"], VERSIONS[0], "{older}");
        }
    }

    #[test]
    fn every_call_is_a_tool_with_a_name_any_client_takes() {
        let r = reply(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#, never);
        let tools = r["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), METHODS.len() + 1);
        assert_eq!(tools[METHODS.len()]["name"], WHY);
        for (t, m) in tools.iter().zip(METHODS) {
            let name = t["name"].as_str().unwrap();
            // Claude's API takes ^[a-zA-Z0-9_-]{1,64}$.
            assert!(
                name.len() <= 64
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "{name}"
            );
            assert_eq!(t["inputSchema"], (m.params)());
        }
    }

    #[test]
    fn a_tool_call_is_forwarded_as_its_method() {
        let line = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"change_get","arguments":{"repo":"o/r","pr":1}}}"#;
        let r = reply(line, |method, params| {
            assert_eq!(
                (method, params),
                ("change.get", &json!({"repo": "o/r", "pr": 1}))
            );
            Ok(json!({"pr": 1}))
        });
        assert_eq!(r["id"], 2);
        assert_eq!(r["result"]["isError"], false);
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(text).unwrap(),
            json!({"pr": 1})
        );
    }

    #[test]
    fn a_call_that_fails_is_the_tools_error_and_an_unknown_tool_the_protocols() {
        let line = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"change_list","arguments":{"since":"x"}}}"#;
        let r = reply(line, |_, _| Err("conaryd: invalid type".into()));
        assert_eq!(r["result"]["isError"], true);
        assert_eq!(r["result"]["content"][0]["text"], "conaryd: invalid type");
        // why's own arguments are checked here, before git or conaryd.
        for arguments in [
            json!({}),
            json!({"dir": "relative", "path": "f", "line": 1}),
            json!({"dir": "/", "path": "f", "line": 1, "repo": "o/r"}),
        ] {
            let line = json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
                "params": {"name": "why", "arguments": arguments}});
            let r = reply(&line.to_string(), never);
            assert_eq!(r["result"]["isError"], true, "{arguments}");
        }
        let line =
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"change.get"}}"#;
        assert_eq!(reply(line, never)["error"]["code"], Error::INVALID_PARAMS);
    }

    #[test]
    fn notifications_get_nothing_and_unknown_methods_an_error() {
        assert!(
            answer(
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                never
            )
            .is_none()
        );
        let r = reply(
            r#"{"jsonrpc":"2.0","id":5,"method":"resources/list"}"#,
            never,
        );
        assert_eq!(r["error"]["code"], Error::NO_METHOD);
        assert_eq!(
            reply(r#"{"jsonrpc":"2.0","id":6,"method":"ping"}"#, never)["result"],
            json!({})
        );
    }
}
