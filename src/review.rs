//! App Review's test mailbox. pstd's Claude feature can't be seen without a server, so the person
//! can open a second, separate mailbox on their own one: `pstd-bridge review`. It has its own pair
//! of tokens and a short link with a code; whoever connects through it gets one test post, and it
//! keeps nothing they send. The person's own mailbox, posts and keys are never reached with it.

use crate::store::{Placement, Post};
use chrono::DateTime;
use rand::Rng;

pub const POST: &str = "pstd-review-test-post";
pub const PICTURE: &str = "pstd-review-test-picture";
pub const JPEG: &[u8] = include_bytes!("../assets/review.jpg");

/// no 0/o, 1/l/i: read off a screen and typed by hand
const LETTERS: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";

pub fn code() -> String {
    let mut rng = rand::thread_rng();
    (0..8).map(|_| LETTERS[rng.gen_range(0..LETTERS.len())] as char).collect()
}

/// the test post, for tomorrow at this hour, so it always lands ahead. Its project is none of
/// the reviewer's: pstd puts it in their first project that plans for one of its networks
pub fn post() -> Post {
    let hour = 60 * 60;
    let at = DateTime::from_timestamp((chrono::Utc::now().timestamp() / hour + 24) * hour, 0)
        .map(|t| t.to_rfc3339())
        .unwrap_or_default();
    let place = |network: &str, caption: &str| Placement {
        network: network.into(),
        account: None,
        format: "post".into(),
        at: at.clone(),
        caption: caption.into(),
    };
    Post {
        id: POST.into(),
        project: "app-review".into(),
        idea: "Test post from the review server".into(),
        prompt: String::new(),
        together: false,
        media: vec![PICTURE.into()],
        placements: vec![
            place("instagram", "Test post from the pstd review server. It came in the way posts from the person's own Claude do. #pstd"),
            place("tiktok", "Test post from the pstd review server. #pstd"),
            place("threads", "Test post from the pstd review server: a picture and a caption per network, scheduled for tomorrow."),
        ],
    }
}

/// the page the short link opens: it hands the link to pstd at once, with a button if the
/// browser holds it back
pub fn page(link: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>pstd · test server</title>\
<style>body{{font:17px -apple-system,system-ui,sans-serif;margin:0;padding:48px 24px;text-align:center;color:#222;background:#f6f3ef}}\
a{{display:inline-block;margin-top:24px;padding:14px 28px;border-radius:999px;background:#222;color:#fff;text-decoration:none;font-weight:600}}\
p{{color:#666}}</style></head><body><h1>pstd test server</h1>\
<p>Open pstd and confirm “Connect This Server?”.</p><a href=\"{link}\">Open in pstd</a>\
<script>location.href={json}</script></body></html>",
        link = link.replace('&', "&amp;"),
        json = serde_json::to_string(link).unwrap_or_default(),
    )
}
