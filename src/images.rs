//! Pictures made with the person's own key, kept on the server so one place holds it whichever
//! Claude runs. The first generator is OpenAI's Images API; the model is in the config.

use crate::store::Store;
use base64::Engine;
use futures::future::BoxFuture;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_MODEL: &str = "gpt-image-1";

pub type Maker = Arc<dyn Fn(String, String) -> BoxFuture<'static, Result<Vec<u8>, String>> + Send + Sync>;

/// the generator's own size nearest to the aspect; pstd crops the rest
fn size(aspect: &str) -> &'static str {
    match aspect {
        "4:5" | "9:16" | "2:3" => "1024x1536",
        "16:9" | "3:2" => "1536x1024",
        _ => "1024x1024",
    }
}

pub fn openai(store: Arc<Store>) -> Maker {
    Arc::new(move |prompt, aspect| {
        let store = store.clone();
        Box::pin(async move {
            let config = store.config().map_err(|e| e.to_string())?;
            if config.image_key.is_empty() {
                return Err("no image key on the server: run `pstd-bridge key`".into());
            }
            let model = if config.image_model.is_empty() { DEFAULT_MODEL.to_string() } else { config.image_model };
            let reply: Value = reqwest::Client::new()
                .post("https://api.openai.com/v1/images/generations")
                .bearer_auth(config.image_key)
                .timeout(Duration::from_secs(180))
                .json(&json!({ "model": model, "prompt": prompt, "size": size(&aspect), "n": 1 }))
                .send()
                .await
                .map_err(|e| format!("the generator can't be reached: {}", e.without_url()))?
                .json()
                .await
                .map_err(|e| format!("the generator's reply can't be read: {}", e.without_url()))?;
            if let Some(message) = reply["error"]["message"].as_str() {
                return Err(format!("the generator refused: {message}"));
            }
            let data = reply["data"][0]["b64_json"].as_str().ok_or("the generator sent no picture")?;
            base64::engine::general_purpose::STANDARD.decode(data).map_err(|e| e.to_string())
        })
    })
}
