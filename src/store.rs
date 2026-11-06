//! Everything the bridge keeps, as plain files in one folder: the config, the snapshot pstd
//! publishes, the posts waiting for pstd, and their pictures. A post and its pictures are gone once
//! pstd has taken them.

use chrono::{DateTime, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

/// pictures no post came to use go after this long
const ORPHAN_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// taken ids remembered, so a late repeat of a taken post is refused and not delivered twice
const TAKEN_KEPT: usize = 2000;

pub const NETWORKS: [&str; 3] = ["instagram", "tiktok", "threads"];
pub const FORMATS: [&str; 2] = ["post", "story"];

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Config {
    pub pstd_token: String,
    pub claude_token: String,
    /// the address pstd and Claude reach this server at
    #[serde(default)]
    pub url: String,
    /// the hostname its own certificate is for; empty behind another web server
    #[serde(default)]
    pub tls_host: String,
    /// where it listens without a certificate of its own
    #[serde(default)]
    pub bind: String,
    #[serde(default)]
    pub image_key: String,
    #[serde(default)]
    pub image_model: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Placement {
    pub network: String,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default = "default_format")]
    pub format: String,
    pub at: String,
    #[serde(default)]
    pub caption: String,
}

fn default_format() -> String {
    "post".into()
}

/// one post as Claude queues it and pstd takes it (CLAUDE_BRIDGE.md › A post)
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Post {
    pub id: String,
    pub project: String,
    #[serde(default)]
    pub idea: String,
    #[serde(default)]
    pub prompt: String,
    /// in this order; the first is the cover
    #[serde(default)]
    pub media: Vec<String>,
    pub placements: Vec<Placement>,
}

#[derive(Serialize, Deserialize, Default)]
struct Taken {
    ids: Vec<String>,
}

pub struct Store {
    root: PathBuf,
    /// one writer at a time: every change reads, checks, then writes
    gate: Mutex<()>,
}

pub fn token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// an id becomes a file name: nothing that could leave the folder
fn safe(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(&tmp, path)
}

fn kind(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some(("png", "image/png")),
        [0xFF, 0xD8, 0xFF, ..] => Some(("jpg", "image/jpeg")),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some(("webp", "image/webp")),
        _ => None,
    }
}

impl Store {
    pub fn open(root: impl Into<PathBuf>) -> std::io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("posts"))?;
        fs::create_dir_all(root.join("media"))?;
        Ok(Self { root, gate: Mutex::new(()) })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// the config, made with fresh tokens the first time
    pub fn config(&self) -> std::io::Result<Config> {
        let path = self.root.join("config.json");
        if let Ok(data) = fs::read(&path) {
            if let Ok(config) = serde_json::from_slice(&data) {
                return Ok(config);
            }
        }
        let config = Config { pstd_token: token(), claude_token: token(), ..Default::default() };
        self.save_config(&config)?;
        Ok(config)
    }

    pub fn save_config(&self, config: &Config) -> std::io::Result<()> {
        write(&self.root.join("config.json"), &serde_json::to_vec_pretty(config)?)
    }

    pub fn put_snapshot(&self, snapshot: &Value) -> Result<(), String> {
        if !snapshot["projects"].is_array() {
            return Err("a snapshot needs projects".into());
        }
        let _gate = self.gate.lock().unwrap();
        write(&self.root.join("snapshot.json"), snapshot.to_string().as_bytes()).map_err(|e| e.to_string())
    }

    pub fn snapshot(&self) -> Option<Value> {
        serde_json::from_slice(&fs::read(self.root.join("snapshot.json")).ok()?).ok()
    }

    fn taken(&self) -> Taken {
        fs::read(self.root.join("taken.json")).ok().and_then(|d| serde_json::from_slice(&d).ok()).unwrap_or_default()
    }

    fn check(&self, post: &Post) -> Result<(), String> {
        if !safe(&post.id) {
            return Err("id: letters, digits, - and _ only, up to 64".into());
        }
        let snapshot = self.snapshot().ok_or("pstd has not connected yet: open pstd once")?;
        let empty = vec![];
        let projects = snapshot["projects"].as_array().unwrap_or(&empty);
        let project = projects
            .iter()
            .find(|p| p["id"].as_str() == Some(&post.project))
            .ok_or_else(|| format!("no project {}: take the id from pstd_plan", post.project))?;
        if post.placements.is_empty() {
            return Err("a post needs at least one placement".into());
        }
        let networks = project["networks"].as_array().unwrap_or(&empty);
        let accounts = project["accounts"].as_array().unwrap_or(&empty);
        for p in &post.placements {
            if !NETWORKS.contains(&p.network.as_str()) {
                return Err(format!("network {}: one of {NETWORKS:?}", p.network));
            }
            if !networks.iter().any(|n| n.as_str() == Some(&p.network)) {
                return Err(format!("the project does not plan for {}", p.network));
            }
            if !FORMATS.contains(&p.format.as_str()) {
                return Err(format!("format {}: one of {FORMATS:?}", p.format));
            }
            if p.format == "story" && p.network != "instagram" {
                return Err("only Instagram has stories".into());
            }
            DateTime::parse_from_rfc3339(&p.at).map_err(|_| format!("at {}: RFC 3339 with its offset", p.at))?;
            if let Some(account) = &p.account {
                let known = accounts.iter().any(|a| a["id"].as_str() == Some(account) && a["network"].as_str() == Some(&p.network));
                if !known {
                    return Err(format!("no {} account {account}: take the id from pstd_plan", p.network));
                }
            }
            if p.caption.trim().is_empty() && post.media.is_empty() {
                return Err("a placement needs a caption or the post needs media".into());
            }
        }
        for id in &post.media {
            if self.media_path(id).is_none() {
                return Err(format!("no picture {id}: make it with pstd_generate_image"));
            }
        }
        if self.taken().ids.contains(&post.id) {
            return Err("pstd already took this post: use a new id".into());
        }
        Ok(())
    }

    /// queues a post; the same id replaces it until pstd has taken it
    pub fn add_post(&self, post: &Post) -> Result<(), String> {
        let _gate = self.gate.lock().unwrap();
        self.check(post)?;
        let data = serde_json::to_vec_pretty(post).map_err(|e| e.to_string())?;
        write(&self.root.join("posts").join(format!("{}.json", post.id)), &data).map_err(|e| e.to_string())
    }

    /// the posts waiting, oldest first
    pub fn posts(&self) -> Vec<Post> {
        let mut found: Vec<(SystemTime, Post)> = fs::read_dir(self.root.join("posts"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| {
                let post = serde_json::from_slice(&fs::read(e.path()).ok()?).ok()?;
                Some((e.metadata().ok()?.modified().ok()?, post))
            })
            .collect();
        found.sort_by_key(|(when, _)| *when);
        found.into_iter().map(|(_, post)| post).collect()
    }

    /// pstd has the post: it and its pictures go, unless another post uses a picture. Repeating is fine
    pub fn take(&self, id: &str) -> Result<(), String> {
        if !safe(id) {
            return Err("bad id".into());
        }
        let _gate = self.gate.lock().unwrap();
        let path = self.root.join("posts").join(format!("{id}.json"));
        let post: Option<Post> = fs::read(&path).ok().and_then(|d| serde_json::from_slice(&d).ok());
        let _ = fs::remove_file(&path);
        if let Some(post) = post {
            let kept: HashSet<String> = self.posts().into_iter().flat_map(|p| p.media).collect();
            for media in post.media.iter().filter(|m| !kept.contains(*m)) {
                if let Some(file) = self.media_path(media) {
                    let _ = fs::remove_file(file);
                }
            }
        }
        let mut taken = self.taken();
        if !taken.ids.iter().any(|t| t == id) {
            taken.ids.push(id.to_string());
            let extra = taken.ids.len().saturating_sub(TAKEN_KEPT);
            taken.ids.drain(..extra);
            let data = serde_json::to_vec(&taken).map_err(|e| e.to_string())?;
            write(&self.root.join("taken.json"), &data).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn media_path(&self, id: &str) -> Option<PathBuf> {
        if !safe(id) {
            return None;
        }
        ["png", "jpg", "webp"].iter().map(|x| self.root.join("media").join(format!("{id}.{x}"))).find(|p| p.exists())
    }

    pub fn save_media(&self, bytes: &[u8]) -> Result<String, String> {
        let (ext, _) = kind(bytes).ok_or("not a PNG, JPEG or WebP")?;
        self.sweep();
        let id = format!("m_{}", &token()[..24]);
        write(&self.root.join("media").join(format!("{id}.{ext}")), bytes).map_err(|e| e.to_string())?;
        Ok(id)
    }

    pub fn media(&self, id: &str) -> Option<(Vec<u8>, &'static str)> {
        let bytes = fs::read(self.media_path(id)?).ok()?;
        let (_, mime) = kind(&bytes)?;
        Some((bytes, mime))
    }

    fn sweep(&self) {
        let used: HashSet<String> = self.posts().into_iter().flat_map(|p| p.media).collect();
        let now = SystemTime::now();
        for entry in fs::read_dir(self.root.join("media")).into_iter().flatten().flatten() {
            let stem = entry.path().file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let old = entry.metadata().ok().and_then(|m| m.modified().ok()).and_then(|m| now.duration_since(m).ok()).is_some_and(|age| age > ORPHAN_AFTER);
            if old && !used.contains(&stem) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    pub fn stamp() -> String {
        Utc::now().to_rfc3339()
    }
}
