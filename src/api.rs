//! pstd's side of the server: it publishes its snapshot and takes the posts. MCP is Claude's side
//! (`mcp.rs`). Each side has its own token, so one can be replaced without the other.

use crate::images::Maker;
use crate::mcp;
use crate::review;
use crate::setup;
use crate::store::Store;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::Extension;
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use futures::StreamExt;
use serde_json::{json, Value};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

const MAX_BODY: usize = 8 * 1024 * 1024;

#[derive(Clone)]
pub struct Bridge {
    pub store: Arc<Store>,
    pub images: Maker,
}

fn bearer(request: &Request) -> &[u8] {
    request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("")
        .as_bytes()
}

fn matches(given: &[u8], token: &str) -> bool {
    !token.is_empty() && bool::from(given.ct_eq(token.as_bytes()))
}

/// a request made with App Review's token: it reaches the test mailbox only (`review.rs`)
#[derive(Clone, Copy)]
pub struct Sandbox;

type Test = Option<Extension<Sandbox>>;

async fn pstd_only(State(bridge): State<Bridge>, mut request: Request, next: Next) -> Response {
    let Ok(config) = bridge.store.config() else { return StatusCode::UNAUTHORIZED.into_response() };
    let given = bearer(&request).to_vec();
    if matches(&given, &config.pstd_token) {
        return next.run(request).await;
    }
    if config.review.on() && matches(&given, &config.review.pstd_token) {
        request.extensions_mut().insert(Sandbox);
        return next.run(request).await;
    }
    StatusCode::UNAUTHORIZED.into_response()
}

async fn claude_only(State(bridge): State<Bridge>, request: Request, next: Next) -> Response {
    let token = bridge.store.config().map(|c| c.claude_token).unwrap_or_default();
    if matches(bearer(&request), &token) { next.run(request).await } else { StatusCode::UNAUTHORIZED.into_response() }
}

async fn health(State(bridge): State<Bridge>, test: Test) -> Json<Value> {
    let images = test.is_none() && bridge.store.config().is_ok_and(|c| !c.image_key.is_empty());
    Json(json!({ "ok": true, "version": env!("CARGO_PKG_VERSION"), "images": images }))
}

/// the person types the generator's key in pstd, not in a terminal
async fn image_key(State(bridge): State<Bridge>, test: Test, Json(body): Json<Value>) -> Response {
    let Some(key) = body["key"].as_str().map(str::trim).filter(|k| !k.is_empty()) else {
        return (StatusCode::BAD_REQUEST, "a key is needed").into_response();
    };
    // the test mailbox holds no one's key
    if test.is_some() {
        return StatusCode::NO_CONTENT.into_response();
    }
    let saved = bridge.store.config().and_then(|mut config| {
        config.image_key = key.to_string();
        bridge.store.save_config(&config)
    });
    match saved {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

async fn put_snapshot(State(bridge): State<Bridge>, test: Test, Json(snapshot): Json<Value>) -> Response {
    // the reviewer's plan is answered and dropped: it never meets the person's Claude
    if test.is_some() {
        return if snapshot["projects"].is_array() { StatusCode::NO_CONTENT } else { StatusCode::BAD_REQUEST }.into_response();
    }
    match bridge.store.put_snapshot(&snapshot) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(message) => (StatusCode::BAD_REQUEST, message).into_response(),
    }
}

async fn posts(State(bridge): State<Bridge>, test: Test) -> Json<Value> {
    if test.is_some() {
        return Json(json!({ "posts": [review::post()] }));
    }
    Json(json!({ "posts": bridge.store.posts() }))
}

async fn media(State(bridge): State<Bridge>, test: Test, Path(id): Path<String>) -> Response {
    if test.is_some() {
        if id != review::PICTURE {
            return StatusCode::NOT_FOUND.into_response();
        }
        return ([(header::CONTENT_TYPE, "image/jpeg")], review::JPEG).into_response();
    }
    let Some((path, length, mime)) = bridge.store.media(&id) else { return StatusCode::NOT_FOUND.into_response() };
    let Ok(file) = tokio::fs::File::open(path).await else { return StatusCode::NOT_FOUND.into_response() };
    ([(header::CONTENT_TYPE, mime.to_string()), (header::CONTENT_LENGTH, length.to_string())], Body::from_stream(ReaderStream::new(file))).into_response()
}

/// a picture or video Claude made on its own machine: the raw file in, a media id out, so its
/// bytes never pass through the conversation. Any size: it goes straight to disk
async fn upload(State(bridge): State<Bridge>, body: Body) -> Response {
    let (incoming, file) = match bridge.store.incoming() {
        Ok(opened) => opened,
        Err(error) => return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    };
    let mut file = tokio::fs::File::from_std(file);
    let mut body = body.into_data_stream();
    while let Some(chunk) = body.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => return (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
        };
        if let Err(error) = file.write_all(&chunk).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
        }
    }
    if let Err(error) = file.flush().await {
        return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
    }
    drop(file);
    match bridge.store.adopt(&incoming) {
        Ok(id) => Json(json!({ "media": id })).into_response(),
        Err(message) => (StatusCode::BAD_REQUEST, message).into_response(),
    }
}

async fn taken(State(bridge): State<Bridge>, test: Test, Path(id): Path<String>) -> Response {
    // the test post stays for the next device
    if test.is_some() {
        return StatusCode::NO_CONTENT.into_response();
    }
    match bridge.store.take(&id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(message) => (StatusCode::BAD_REQUEST, message).into_response(),
    }
}

/// the short link for App Review: the right code opens the page that hands pstd the test
/// mailbox; anything else, or the mailbox off, is not found
async fn review_link(State(bridge): State<Bridge>, Path(code): Path<String>) -> Response {
    let Ok(config) = bridge.store.config() else { return StatusCode::NOT_FOUND.into_response() };
    if !config.review.on() || !matches(code.to_ascii_lowercase().as_bytes(), &config.review.code) {
        return StatusCode::NOT_FOUND.into_response();
    }
    Html(review::page(&setup::review_link(&config))).into_response()
}

pub fn router(bridge: Bridge) -> Router {
    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/snapshot", put(put_snapshot))
        .route("/api/image-key", put(image_key))
        .route("/api/posts", get(posts))
        .route("/api/media/{id}", get(media))
        .route("/api/posts/{id}/taken", post(taken))
        .route_layer(middleware::from_fn_with_state(bridge.clone(), pstd_only));
    let claude = Router::new()
        .route("/mcp", post(mcp::handle).get(mcp::no_stream).delete(mcp::no_stream))
        .route("/mcp/media", post(upload).layer(DefaultBodyLimit::disable()))
        .route_layer(middleware::from_fn_with_state(bridge.clone(), claude_only));
    let open = Router::new().route("/r/{code}", get(review_link));
    app.merge(claude).merge(open).layer(DefaultBodyLimit::max(MAX_BODY)).with_state(bridge)
}
