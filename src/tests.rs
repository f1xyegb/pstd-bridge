use crate::api::{router, Bridge};
use crate::setup;
use crate::store::{Post, Store};
use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 1, 2, 3];
const P: &str = "aaaaaaaa-0000-0000-0000-000000000001";
const A: &str = "bbbbbbbb-0000-0000-0000-000000000002";

struct Rig {
    app: Router,
    store: Arc<Store>,
    pstd: String,
    claude: String,
    _dir: tempfile::TempDir,
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let config = store.config().unwrap();
    let images = Arc::new(|prompt: String, _: String| {
        Box::pin(async move { if prompt == "fail" { Err("no key".to_string()) } else { Ok(PNG.to_vec()) } })
            as futures::future::BoxFuture<'static, Result<Vec<u8>, String>>
    });
    Rig { app: router(Bridge { store: store.clone(), images }), store, pstd: config.pstd_token, claude: config.claude_token, _dir: dir }
}

async fn send(app: &Router, method: Method, uri: &str, token: &str, body: Option<Value>) -> (StatusCode, Vec<u8>) {
    let request = Request::builder().method(method).uri(uri).header(header::AUTHORIZATION, format!("Bearer {token}"));
    let request = match body {
        Some(b) => request.header(header::CONTENT_TYPE, "application/json").body(Body::from(b.to_string())),
        None => request.body(Body::empty()),
    };
    let reply = app.clone().oneshot(request.unwrap()).await.unwrap();
    let status = reply.status();
    (status, reply.into_body().collect().await.unwrap().to_bytes().to_vec())
}

async fn publish(r: &Rig) {
    let snapshot = json!({ "projects": [{ "id": P, "name": "Oat & Ember", "networks": ["instagram", "threads"],
        "accounts": [{ "id": A, "network": "instagram", "handle": "oatandember" }] }] });
    assert_eq!(send(&r.app, Method::PUT, "/api/snapshot", &r.pstd, Some(snapshot)).await.0, StatusCode::NO_CONTENT);
}

async fn tool(r: &Rig, name: &str, arguments: Value) -> (Value, bool) {
    let message = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": arguments } });
    let (status, body) = send(&r.app, Method::POST, "/mcp", &r.claude, Some(message)).await;
    assert_eq!(status, StatusCode::OK);
    let reply: Value = serde_json::from_slice(&body).unwrap();
    let text = reply["result"]["content"][0]["text"].as_str().unwrap().to_string();
    let failed = reply["result"]["isError"].as_bool().unwrap_or(false);
    (serde_json::from_str(&text).unwrap_or(Value::String(text)), failed)
}

fn post(id: &str, media: Vec<String>) -> Value {
    json!({ "id": id, "project": P, "idea": "mugs", "media": media,
        "placements": [{ "network": "instagram", "account": A, "at": "2026-11-12T19:00:00+01:00", "caption": "New mugs" }] })
}

#[tokio::test]
async fn each_side_has_its_own_token() {
    let r = rig();
    assert_eq!(send(&r.app, Method::GET, "/api/posts", "", None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(send(&r.app, Method::GET, "/api/posts", &r.claude, None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(send(&r.app, Method::POST, "/mcp", &r.pstd, Some(json!({}))).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(send(&r.app, Method::GET, "/api/health", &r.pstd, None).await.0, StatusCode::OK);
}

#[tokio::test]
async fn a_post_goes_from_claude_to_pstd_in_order() {
    let r = rig();
    publish(&r).await;
    let (plan, failed) = tool(&r, "pstd_plan", json!({})).await;
    assert!(!failed);
    assert_eq!(plan["projects"][0]["id"], P);

    let first = tool(&r, "pstd_generate_image", json!({ "prompt": "a mug", "aspect": "4:5" })).await.0["media"].as_str().unwrap().to_string();
    let second = tool(&r, "pstd_generate_image", json!({ "prompt": "a shelf" })).await.0["media"].as_str().unwrap().to_string();
    let (_, failed) = tool(&r, "pstd_add_post", post("p1", vec![first.clone(), second.clone()])).await;
    assert!(!failed);

    let (_, body) = send(&r.app, Method::GET, "/api/posts", &r.pstd, None).await;
    let posts: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(posts["posts"][0]["media"], json!([first, second]));
    assert_eq!(posts["posts"][0]["placements"][0]["format"], "post");

    let (status, bytes) = send(&r.app, Method::GET, &format!("/api/media/{first}"), &r.pstd, None).await;
    assert_eq!((status, bytes.as_slice()), (StatusCode::OK, PNG));

    assert_eq!(send(&r.app, Method::POST, "/api/posts/p1/taken", &r.pstd, None).await.0, StatusCode::NO_CONTENT);
    assert_eq!(send(&r.app, Method::POST, "/api/posts/p1/taken", &r.pstd, None).await.0, StatusCode::NO_CONTENT);
    assert!(r.store.posts().is_empty());
    assert_eq!(send(&r.app, Method::GET, &format!("/api/media/{first}"), &r.pstd, None).await.0, StatusCode::NOT_FOUND);

    let (message, failed) = tool(&r, "pstd_add_post", post("p1", vec![])).await;
    assert!(failed, "{message}");
}

#[tokio::test]
async fn a_shared_picture_outlives_the_first_post_taken() {
    let r = rig();
    publish(&r).await;
    let media = r.store.save_media(PNG).unwrap();
    tool(&r, "pstd_add_post", post("a", vec![media.clone()])).await;
    tool(&r, "pstd_add_post", post("b", vec![media.clone()])).await;
    r.store.take("a").unwrap();
    assert!(r.store.media(&media).is_some());
    r.store.take("b").unwrap();
    assert!(r.store.media(&media).is_none());
}

#[tokio::test]
async fn the_same_id_replaces_until_taken() {
    let r = rig();
    publish(&r).await;
    tool(&r, "pstd_add_post", post("p", vec![])).await;
    let mut edited = post("p", vec![]);
    edited["idea"] = json!("cups");
    tool(&r, "pstd_add_post", edited).await;
    let posts = r.store.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].idea, "cups");
}

#[tokio::test]
async fn wrong_posts_are_refused_with_a_reason() {
    let r = rig();
    let (message, failed) = tool(&r, "pstd_add_post", post("p", vec![])).await;
    assert!(failed && message.as_str().unwrap().contains("open pstd"));
    publish(&r).await;
    for (change, expected) in [
        (json!({ "id": "../evil" }), "id:"),
        (json!({ "project": "nope" }), "no project"),
        (json!({ "media": ["m_none"] }), "no picture"),
    ] {
        let mut bad = post("p", vec![]);
        bad.as_object_mut().unwrap().extend(change.as_object().unwrap().clone());
        let (message, failed) = tool(&r, "pstd_add_post", bad).await;
        assert!(failed && message.as_str().unwrap().contains(expected), "{message}");
    }
    for (field, value, expected) in [
        ("network", "tiktok", "does not plan"),
        ("network", "myspace", "one of"),
        ("at", "tomorrow", "RFC 3339"),
        ("account", "nope", "no instagram account"),
        ("format", "reel", "one of"),
    ] {
        let mut bad = post("p", vec![]);
        bad["placements"][0][field] = json!(value);
        let (message, failed) = tool(&r, "pstd_add_post", bad).await;
        assert!(failed && message.as_str().unwrap().contains(expected), "{message}");
    }
    let mut story = post("p", vec![]);
    story["placements"][0] = json!({ "network": "threads", "format": "story", "at": "2026-11-12T19:00:00+01:00", "caption": "x" });
    assert!(tool(&r, "pstd_add_post", story).await.1);
    assert!(r.store.posts().is_empty());
}

#[tokio::test]
async fn a_generator_failure_reaches_claude_as_text() {
    let r = rig();
    let (message, failed) = tool(&r, "pstd_generate_image", json!({ "prompt": "fail" })).await;
    assert!(failed);
    assert_eq!(message, "no key");
}

#[tokio::test]
async fn mcp_handshake() {
    let r = rig();
    let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } });
    let reply: Value = serde_json::from_slice(&send(&r.app, Method::POST, "/mcp", &r.claude, Some(init)).await.1).unwrap();
    assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
    let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    assert_eq!(send(&r.app, Method::POST, "/mcp", &r.claude, Some(note)).await.0, StatusCode::ACCEPTED);
    let list = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
    let reply: Value = serde_json::from_slice(&send(&r.app, Method::POST, "/mcp", &r.claude, Some(list)).await.1).unwrap();
    assert_eq!(reply["result"]["tools"].as_array().unwrap().len(), 4);
    assert_eq!(send(&r.app, Method::GET, "/mcp", &r.claude, None).await.0, StatusCode::METHOD_NOT_ALLOWED);
}

#[test]
fn the_pair_link_is_escaped() {
    let config = crate::store::Config { url: "https://1-2-3-4.sslip.io".into(), pstd_token: "abc".into(), claude_token: "def".into(), ..Default::default() };
    assert_eq!(setup::pair_link(&config), "pstd://connect?url=https%3A%2F%2F1-2-3-4.sslip.io&token=abc&claude=def");
}

#[tokio::test]
async fn pstd_sets_the_image_key() {
    let r = rig();
    let (_, body) = send(&r.app, Method::GET, "/api/health", &r.pstd, None).await;
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["images"], false);
    assert_eq!(send(&r.app, Method::PUT, "/api/image-key", &r.claude, Some(json!({ "key": "k" }))).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(send(&r.app, Method::PUT, "/api/image-key", &r.pstd, Some(json!({ "key": " " }))).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(send(&r.app, Method::PUT, "/api/image-key", &r.pstd, Some(json!({ "key": "k" }))).await.0, StatusCode::NO_CONTENT);
    let (_, body) = send(&r.app, Method::GET, "/api/health", &r.pstd, None).await;
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["images"], true);
    assert_eq!(r.store.config().unwrap().pstd_token, r.pstd);
}

#[test]
fn behind_a_website_the_address_is_a_path_under_its_domain() {
    assert_eq!(setup::site_url("example.com"), "https://example.com/pstd-bridge");
}

#[test]
fn a_post_reads_without_its_optional_fields() {
    let p: Post = serde_json::from_value(json!({ "id": "x", "project": "y", "placements": [{ "network": "threads", "at": "z" }] })).unwrap();
    assert_eq!(p.placements[0].format, "post");
}
