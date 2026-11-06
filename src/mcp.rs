//! Claude's side: MCP over plain HTTP, each request answered with one JSON reply. Four tools
//! (CLAUDE_BRIDGE.md › MCP tools). A tool's failure comes back as its own result, written for
//! Claude to act on, not as a protocol error.

use crate::api::Bridge;
use crate::store::{Post, Store};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

const VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

fn tools() -> Value {
    json!([
        {
            "name": "pstd_plan",
            "description": "The person's plan in pstd: projects, their networks, accounts, posting slots, the dates already taken, and the posts queued here. Read this first: every id comes from it.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "pstd_generate_image",
            "description": "Makes one picture with the person's generator and returns its media id for pstd_add_post. Make pictures in the order they go in the post.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "prompt": { "type": "string" },
                    "aspect": { "type": "string", "enum": ["1:1", "4:5", "9:16", "16:9"], "description": "4:5 for an Instagram post, 9:16 for a story or TikTok" }
                },
                "required": ["prompt"]
            }
        },
        {
            "name": "pstd_add_post",
            "description": "Queues one post for pstd. One placement per network, each with its own caption. Repeating an id replaces that post until pstd has taken it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "your own unique id: letters, digits, - and _" },
                    "project": { "type": "string", "description": "a project id from pstd_plan" },
                    "idea": { "type": "string" },
                    "prompt": { "type": "string", "description": "what the pictures were made from" },
                    "media": { "type": "array", "items": { "type": "string" }, "description": "media ids in order; the first is the cover" },
                    "placements": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "network": { "type": "string", "enum": ["instagram", "tiktok", "threads"] },
                                "account": { "type": "string", "description": "an account id from pstd_plan; leave out for the first" },
                                "format": { "type": "string", "enum": ["post", "story"] },
                                "at": { "type": "string", "description": "RFC 3339 with the person's offset" },
                                "caption": { "type": "string" }
                            },
                            "required": ["network", "at"]
                        }
                    }
                },
                "required": ["id", "project", "placements"]
            }
        },
        {
            "name": "pstd_queue",
            "description": "The posts still waiting for pstd.",
            "inputSchema": { "type": "object", "properties": {} }
        }
    ])
}

fn summary(store: &Store) -> Value {
    json!(store.posts().iter().map(|p| json!({
        "id": p.id,
        "project": p.project,
        "placements": p.placements.iter().map(|x| json!({ "network": x.network, "at": x.at })).collect::<Vec<_>>(),
    })).collect::<Vec<_>>())
}

async fn call(bridge: &Bridge, name: &str, input: Value) -> Result<Value, String> {
    let store = &bridge.store;
    match name {
        "pstd_plan" => {
            let snapshot = store.snapshot().ok_or("pstd has not connected yet: open pstd once")?;
            Ok(json!({ "now": Store::stamp(), "projects": snapshot["projects"], "queued": summary(store) }))
        }
        "pstd_queue" => Ok(json!({ "queued": summary(store) })),
        "pstd_add_post" => {
            let post: Post = serde_json::from_value(input).map_err(|e| format!("post: {e}"))?;
            store.add_post(&post)?;
            Ok(json!({ "queued": post.id }))
        }
        "pstd_generate_image" => {
            let prompt = input["prompt"].as_str().filter(|p| !p.trim().is_empty()).ok_or("a prompt is needed")?;
            let aspect = input["aspect"].as_str().unwrap_or("1:1").to_string();
            let bytes = (bridge.images)(prompt.to_string(), aspect).await?;
            Ok(json!({ "media": store.save_media(&bytes)? }))
        }
        _ => Err(format!("no tool {name}")),
    }
}

pub async fn handle(State(bridge): State<Bridge>, Json(message): Json<Value>) -> Response {
    // a notification has no id and wants no reply
    let Some(id) = message.get("id").cloned() else { return StatusCode::ACCEPTED.into_response() };
    let method = message["method"].as_str().unwrap_or("");
    let params = &message["params"];
    let result = match method {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or("");
            let version = VERSIONS.iter().find(|v| **v == asked).unwrap_or(&VERSIONS[0]);
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "pstd", "version": env!("CARGO_PKG_VERSION") }
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or("");
            let input = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            match call(&bridge, name, input).await {
                Ok(value) => json!({ "content": [{ "type": "text", "text": value.to_string() }] }),
                Err(message) => json!({ "content": [{ "type": "text", "text": message }], "isError": true }),
            }
        }
        _ => {
            let error = json!({ "code": -32601, "message": format!("no method {method}") });
            return Json(json!({ "jsonrpc": "2.0", "id": id, "error": error })).into_response();
        }
    };
    Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response()
}

/// no server-to-client stream: everything is answered in its POST
pub async fn no_stream() -> StatusCode {
    StatusCode::METHOD_NOT_ALLOWED
}
