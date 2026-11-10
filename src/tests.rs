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
async fn claude_uploads_a_picture_it_made() {
    let r = rig();
    let upload = |token: String, bytes: &'static [u8]| {
        let app = r.app.clone();
        async move {
            let request = Request::builder().method(Method::POST).uri("/mcp/media").header(header::AUTHORIZATION, format!("Bearer {token}"));
            let reply = app.oneshot(request.body(Body::from(bytes)).unwrap()).await.unwrap();
            let status = reply.status();
            (status, reply.into_body().collect().await.unwrap().to_bytes().to_vec())
        }
    };
    assert_eq!(upload(r.pstd.clone(), PNG).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(upload(r.claude.clone(), b"not a picture").await.0, StatusCode::BAD_REQUEST);
    let (status, body) = upload(r.claude.clone(), PNG).await;
    assert_eq!(status, StatusCode::OK);
    let id = serde_json::from_slice::<Value>(&body).unwrap()["media"].as_str().unwrap().to_string();
    let (status, bytes) = send(&r.app, Method::GET, &format!("/api/media/{id}"), &r.pstd, None).await;
    assert_eq!((status, bytes.as_slice()), (StatusCode::OK, PNG));
}

#[tokio::test]
async fn a_large_video_goes_up_and_comes_back_with_its_type() {
    let r = rig();
    let mut video = b"\0\0\0\x18ftypisom".to_vec();
    video.resize(9 * 1024 * 1024, 7);
    let upload = |bytes: Vec<u8>| {
        let request = Request::builder().method(Method::POST).uri("/mcp/media").header(header::AUTHORIZATION, format!("Bearer {}", r.claude));
        r.app.clone().oneshot(request.body(Body::from(bytes)).unwrap())
    };
    let reply = upload(video.clone()).await.unwrap();
    assert_eq!(reply.status(), StatusCode::OK);
    let body = reply.into_body().collect().await.unwrap().to_bytes();
    let id = serde_json::from_slice::<Value>(&body).unwrap()["media"].as_str().unwrap().to_string();

    let request = Request::builder().uri(format!("/api/media/{id}")).header(header::AUTHORIZATION, format!("Bearer {}", r.pstd));
    let reply = r.app.clone().oneshot(request.body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(reply.status(), StatusCode::OK);
    assert_eq!(reply.headers()[header::CONTENT_TYPE], "video/mp4");
    assert_eq!(reply.headers()[header::CONTENT_LENGTH], video.len().to_string());
    assert_eq!(reply.into_body().collect().await.unwrap().to_bytes().len(), video.len());

    let mov = b"\0\0\0\x14ftypqt  \0\0\0\0".to_vec();
    assert_eq!(upload(mov).await.unwrap().status(), StatusCode::OK);
    let heic = b"\0\0\0\x18ftypheic\0\0\0\0".to_vec();
    assert_eq!(upload(heic).await.unwrap().status(), StatusCode::BAD_REQUEST);
    let left: Vec<_> = std::fs::read_dir(r.store.root().join("media")).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with(".in_")).collect();
    assert!(left.is_empty(), "an upload left its temporary file");
}

#[test]
fn a_file_no_post_used_goes_after_a_day() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let age = |id: &str, hours: u64| {
        let file = std::fs::File::options().write(true).open(dir.path().join("media").join(format!("{id}.png"))).unwrap();
        file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(hours * 60 * 60)).unwrap();
    };
    let (old, fresh) = (store.save_media(PNG).unwrap(), store.save_media(PNG).unwrap());
    age(&old, 25);
    age(&fresh, 23);
    store.sweep();
    assert!(store.media(&old).is_none());
    assert!(store.media(&fresh).is_some());
}

#[tokio::test]
async fn the_json_routes_keep_their_limit() {
    let r = rig();
    let request = Request::builder().method(Method::POST).uri("/mcp").header(header::AUTHORIZATION, format!("Bearer {}", r.claude)).header(header::CONTENT_TYPE, "application/json");
    let reply = r.app.clone().oneshot(request.body(Body::from(vec![b' '; 9 * 1024 * 1024])).unwrap()).await.unwrap();
    assert_eq!(reply.status(), StatusCode::PAYLOAD_TOO_LARGE);
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
fn running_setup_again_asks_nothing_unless_the_domain_changes() {
    let fresh = crate::store::Config::default();
    assert!(!setup::already_set_up(&fresh, None));
    let done = crate::store::Config { url: "https://1-2-3-4.sslip.io".into(), ..Default::default() };
    assert!(setup::already_set_up(&done, None));
    assert!(setup::already_set_up(&done, Some("1-2-3-4.sslip.io")));
    assert!(!setup::already_set_up(&done, Some("example.com")));
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

fn open_review(r: &Rig) -> crate::store::Review {
    let mut config = r.store.config().unwrap();
    config.url = "https://bridge.example".into();
    config.image_key = "the person's key".into();
    config.review = crate::store::Review { pstd_token: crate::store::token(), claude_token: crate::store::token(), code: "abcd2345".into() };
    r.store.save_config(&config).unwrap();
    config.review
}

#[tokio::test]
async fn app_review_gets_the_test_post_and_never_the_persons_mailbox() {
    let r = rig();
    publish(&r).await;
    assert!(!tool(&r, "pstd_add_post", post("real", vec![])).await.1);
    let test = open_review(&r);

    // the test token sees one test post, with its picture, and no image key
    let (status, body) = send(&r.app, Method::GET, "/api/posts", &test.pstd_token, None).await;
    assert_eq!(status, StatusCode::OK);
    let posts: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(posts["posts"].as_array().unwrap().len(), 1);
    assert_eq!(posts["posts"][0]["id"], crate::review::POST);
    let at = posts["posts"][0]["placements"][0]["at"].as_str().unwrap();
    assert!(chrono::DateTime::parse_from_rfc3339(at).unwrap() > chrono::Utc::now());
    let (status, bytes) = send(&r.app, Method::GET, &format!("/api/media/{}", crate::review::PICTURE), &test.pstd_token, None).await;
    assert_eq!((status, bytes.as_slice()), (StatusCode::OK, crate::review::JPEG));
    assert_eq!(send(&r.app, Method::GET, "/api/media/anything-else", &test.pstd_token, None).await.0, StatusCode::NOT_FOUND);
    let (_, health) = send(&r.app, Method::GET, "/api/health", &test.pstd_token, None).await;
    assert_eq!(serde_json::from_slice::<Value>(&health).unwrap()["images"], false);

    // what the reviewer sends is answered and dropped; taking the test post keeps it for the next device
    let theirs = json!({ "projects": [{ "id": "x", "name": "Review", "networks": ["threads"] }] });
    assert_eq!(send(&r.app, Method::PUT, "/api/snapshot", &test.pstd_token, Some(theirs)).await.0, StatusCode::NO_CONTENT);
    assert_eq!(send(&r.app, Method::PUT, "/api/image-key", &test.pstd_token, Some(json!({ "key": "theirs" }))).await.0, StatusCode::NO_CONTENT);
    let taken = format!("/api/posts/{}/taken", crate::review::POST);
    assert_eq!(send(&r.app, Method::POST, &taken, &test.pstd_token, None).await.0, StatusCode::NO_CONTENT);
    let (_, again) = send(&r.app, Method::GET, "/api/posts", &test.pstd_token, None).await;
    assert_eq!(serde_json::from_slice::<Value>(&again).unwrap()["posts"][0]["id"], crate::review::POST);

    // the person's own mailbox is untouched: their plan, key and waiting post
    assert_eq!(r.store.snapshot().unwrap()["projects"][0]["id"], P);
    assert_eq!(r.store.config().unwrap().image_key, "the person's key");
    let (_, mine) = send(&r.app, Method::GET, "/api/posts", &r.pstd, None).await;
    let mine: Value = serde_json::from_slice(&mine).unwrap();
    assert_eq!(mine["posts"].as_array().unwrap().len(), 1);
    assert_eq!(mine["posts"][0]["id"], "real");

    // and the test tokens don't open Claude's side
    assert_eq!(send(&r.app, Method::POST, "/mcp", &test.claude_token, Some(json!({}))).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_review_link_opens_only_with_its_code_and_only_while_open() {
    let r = rig();
    assert_eq!(send(&r.app, Method::GET, "/r/abcd2345", "", None).await.0, StatusCode::NOT_FOUND);
    let test = open_review(&r);
    let (status, page) = send(&r.app, Method::GET, "/r/ABCD2345", "", None).await;
    assert_eq!(status, StatusCode::OK);
    let page = String::from_utf8(page).unwrap();
    assert!(page.contains(&format!("token={}", test.pstd_token)));
    assert!(!page.contains(&r.pstd));
    assert_eq!(send(&r.app, Method::GET, "/r/abcd2346", "", None).await.0, StatusCode::NOT_FOUND);

    let mut config = r.store.config().unwrap();
    config.review = Default::default();
    r.store.save_config(&config).unwrap();
    assert_eq!(send(&r.app, Method::GET, "/r/abcd2345", "", None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(send(&r.app, Method::GET, "/api/posts", &test.pstd_token, None).await.0, StatusCode::UNAUTHORIZED);
}

#[test]
fn a_config_that_cant_be_read_is_an_error_and_is_never_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let first = store.config().unwrap();
    let path = dir.path().join("config.json");

    // half a file, as if read mid-write: an error, and the file stays as it is
    std::fs::write(&path, b"{\"pstd_token\":").unwrap();
    assert!(store.config().is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"{\"pstd_token\":");

    // a file this process may not read: the same
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let whole = serde_json::to_vec(&first).unwrap();
        std::fs::write(&path, &whole).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&path).is_err() {
            assert!(store.config().is_err());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), whole);
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert_eq!(store.config().unwrap().pstd_token, first.pstd_token);
}
