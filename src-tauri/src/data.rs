//! Slay the Spire 2 card/relic data, fetched once from the Spire Codex API
//! (https://spire-codex.com) and cached on disk together with every image.
//!
//! Layout of the cache directory:
//!   data/cards.json, data/relics.json   raw API responses
//!   data/meta.json                      when/where the data came from
//!   images/<kind>/<ID>.<ext>            downloaded images
//!   images/<kind>/<ID>.missing          marker: the server had no such image

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

pub const API_BASE: &str = "https://spire-codex.com";

/// `STS2_API_BASE` overrides the server (for testing against a mirror).
pub fn api_base() -> String {
    std::env::var("STS2_API_BASE").unwrap_or_else(|_| API_BASE.to_string()).trim_end_matches('/').to_string()
}
pub const USER_AGENT: &str = concat!("sts2-callout/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Card {
    pub id: String,
    pub name: String,
    pub description: String,
    pub upgrade_description: Option<String>,
    pub cost: Option<i64>,
    pub is_x_cost: Option<bool>,
    pub star_cost: Option<i64>,
    pub is_x_star_cost: Option<bool>,
    #[serde(rename = "type")]
    pub card_type: String,
    pub rarity: String,
    pub color: String,
    pub keywords: Option<Vec<String>>,
    pub upgrade: Option<serde_json::Value>,
    pub image_url: Option<String>,
    pub image_url_card: Option<String>,
    pub image_url_card_upg: Option<String>,
    pub compendium_order: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Relic {
    pub id: String,
    pub name: String,
    pub description: String,
    pub flavor: Option<String>,
    pub rarity: String,
    pub pool: String,
    pub image_url: Option<String>,
    pub compendium_order: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Meta {
    pub fetched_at: u64,
    pub source: String,
}

pub struct GameData {
    pub cards: Vec<Card>,
    pub relics: Vec<Relic>,
    pub meta: Meta,
}

impl GameData {
    pub fn card(&self, id: &str) -> Option<&Card> {
        self.cards.iter().find(|c| c.id == id)
    }
    pub fn relic(&self, id: &str) -> Option<&Relic> {
        self.relics.iter().find(|r| r.id == id)
    }

    /// Remote URL for an image kind served by our `stsimg://` protocol.
    pub fn image_url(&self, kind: ImageKind, id: &str) -> Option<String> {
        let url = match kind {
            ImageKind::Card => self.card(id)?.image_url_card.clone(),
            ImageKind::CardUpg => self.card(id)?.image_url_card_upg.clone(),
            ImageKind::CardArt => self.card(id)?.image_url.clone(),
            ImageKind::Relic => self.relic(id)?.image_url.clone(),
        }?;
        Some(absolute_url(&url))
    }

    /// Every image the overlay can ask for, used to warm the cache.
    pub fn all_images(&self) -> Vec<(ImageKind, String)> {
        let mut out = Vec::new();
        for c in &self.cards {
            out.push((ImageKind::Card, c.id.clone()));
            if c.image_url_card_upg.is_some() {
                out.push((ImageKind::CardUpg, c.id.clone()));
            }
            out.push((ImageKind::CardArt, c.id.clone()));
        }
        for r in &self.relics {
            out.push((ImageKind::Relic, r.id.clone()));
        }
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Card,
    CardUpg,
    CardArt,
    Relic,
}

impl ImageKind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "card" => Self::Card,
            "card_upg" => Self::CardUpg,
            "card_art" => Self::CardArt,
            "relic" => Self::Relic,
            _ => return None,
        })
    }
    fn dir(self) -> &'static str {
        match self {
            Self::Card => "card",
            Self::CardUpg => "card_upg",
            Self::CardArt => "card_art",
            Self::Relic => "relic",
        }
    }
}

fn absolute_url(url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else {
        format!("{}/{}", api_base(), url.trim_start_matches('/'))
    }
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(std::time::Duration::from_secs(15))
        // per-read, not total: model downloads are hundreds of MB
        .read_timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("http client")
}

/// Load the cached data, hitting the API only if there is no cache yet
/// (or `force` is set by an explicit "refresh" from the user).
pub async fn load_or_fetch(client: &reqwest::Client, cache: &Path, force: bool) -> Result<GameData> {
    let dir = cache.join("data");
    let cards_path = dir.join("cards.json");
    let relics_path = dir.join("relics.json");
    let meta_path = dir.join("meta.json");

    if !force && cards_path.exists() && relics_path.exists() {
        if let Ok(data) = read_cached(&cards_path, &relics_path, &meta_path) {
            return Ok(data);
        }
        log::warn!("cached data unreadable, refetching");
    }

    tokio::fs::create_dir_all(&dir).await?;
    let base = api_base();
    let cards_raw = fetch_text(client, &format!("{base}/api/cards?lang=eng")).await?;
    let relics_raw = fetch_text(client, &format!("{base}/api/relics?lang=eng")).await?;
    // Validate before overwriting a good cache.
    serde_json::from_str::<Vec<Card>>(&cards_raw).context("unexpected /api/cards response")?;
    serde_json::from_str::<Vec<Relic>>(&relics_raw).context("unexpected /api/relics response")?;
    let meta = Meta {
        fetched_at: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        source: base,
    };
    write_atomic(&cards_path, cards_raw.as_bytes()).await?;
    write_atomic(&relics_path, relics_raw.as_bytes()).await?;
    write_atomic(&meta_path, serde_json::to_vec_pretty(&meta)?.as_slice()).await?;
    if force {
        // Data changed: drop "missing" markers so new images get a chance.
        clear_missing_markers(&cache.join("images"));
    }
    read_cached(&cards_path, &relics_path, &meta_path)
}

fn read_cached(cards: &Path, relics: &Path, meta: &Path) -> Result<GameData> {
    let cards: Vec<Card> = serde_json::from_slice(&std::fs::read(cards)?)?;
    let relics: Vec<Relic> = serde_json::from_slice(&std::fs::read(relics)?)?;
    let meta = std::fs::read(meta)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    Ok(GameData { cards, relics, meta })
}

async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<String> {
    let resp = client.get(url).send().await.with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        return Err(anyhow!("GET {url}: HTTP {}", resp.status()));
    }
    Ok(resp.text().await?)
}

/// Write via a uniquely named temp file + rename, so concurrent writers of the
/// same file (prefetch and an on-demand request) never see a partial file.
pub async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("{}-{n}.part", std::process::id()));
    tokio::fs::write(&tmp, bytes).await?;
    if let Err(e) = tokio::fs::rename(&tmp, path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        if !path.exists() {
            return Err(e.into());
        }
    }
    Ok(())
}

fn clear_missing_markers(images: &Path) {
    let Ok(kinds) = std::fs::read_dir(images) else { return };
    for kind in kinds.flatten() {
        let Ok(files) = std::fs::read_dir(kind.path()) else { continue };
        for f in files.flatten() {
            if f.path().extension().is_some_and(|e| e == "missing") {
                let _ = std::fs::remove_file(f.path());
            }
        }
    }
}

pub enum CachedImage {
    Found(PathBuf),
    Missing,
}

fn find_cached(dir: &Path, id: &str) -> Option<CachedImage> {
    for ext in ["webp", "png", "jpg", "gif"] {
        let p = dir.join(format!("{id}.{ext}"));
        if p.exists() {
            return Some(CachedImage::Found(p));
        }
    }
    dir.join(format!("{id}.missing")).exists().then_some(CachedImage::Missing)
}

/// Return the local file for an image, downloading it the first time only.
pub async fn get_image(
    client: &reqwest::Client,
    cache: &Path,
    data: &GameData,
    kind: ImageKind,
    id: &str,
) -> Result<CachedImage> {
    // ids come from the URL, keep them to plain file names
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(anyhow!("bad id"));
    }
    let dir = cache.join("images").join(kind.dir());
    if let Some(hit) = find_cached(&dir, id) {
        return Ok(hit);
    }
    tokio::fs::create_dir_all(&dir).await?;
    let Some(url) = data.image_url(kind, id) else {
        tokio::fs::write(dir.join(format!("{id}.missing")), b"").await?;
        return Ok(CachedImage::Missing);
    };
    let resp = client.get(&url).send().await?;
    // Only a definite "doesn't exist" is remembered; 403/5xx may be temporary.
    if resp.status() == reqwest::StatusCode::NOT_FOUND || resp.status() == reqwest::StatusCode::GONE {
        tokio::fs::write(dir.join(format!("{id}.missing")), b"").await?;
        return Ok(CachedImage::Missing);
    }
    if !resp.status().is_success() {
        return Err(anyhow!("GET {url}: HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().await?;
    let ext = sniff_ext(&bytes).unwrap_or_else(|| {
        url.rsplit('.').next().filter(|e| e.len() <= 4).unwrap_or("webp")
    });
    let path = dir.join(format!("{id}.{ext}"));
    write_atomic(&path, &bytes).await?;
    Ok(CachedImage::Found(path))
}

fn sniff_ext(b: &[u8]) -> Option<&'static str> {
    if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("webp")
    } else if b.starts_with(b"\x89PNG") {
        Some("png")
    } else if b.starts_with(b"\xFF\xD8") {
        Some("jpg")
    } else if b.starts_with(b"GIF8") {
        Some("gif")
    } else {
        None
    }
}

pub fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("png") => "image/png",
        Some("jpg") => "image/jpeg",
        Some("gif") => "image/gif",
        _ => "image/webp",
    }
}

/// Download a file (a speech model) with progress callbacks. Stops early when
/// `cancel` is set; a partial or truncated download never replaces `dest`.
pub async fn download_file(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        return Err(anyhow!("GET {url}: HTTP {}", resp.status()));
    }
    let total = resp.content_length();
    let tmp = dest.with_extension("part");
    let result = async {
        let mut file = tokio::fs::File::create(&tmp).await?;
        let mut done = 0u64;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow!("cancelled"));
            }
            let chunk = chunk?;
            file.write_all(&chunk).await?;
            done += chunk.len() as u64;
            progress(done, total);
        }
        if total.is_some_and(|t| t != done) || done == 0 {
            return Err(anyhow!("download of {url} was cut off ({done} bytes)"));
        }
        file.flush().await?;
        anyhow::Ok(())
    }
    .await;
    match result {
        Ok(()) => {
            tokio::fs::rename(&tmp, dest).await?;
            Ok(())
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            Err(e)
        }
    }
}
