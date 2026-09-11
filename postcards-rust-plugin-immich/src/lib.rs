//! Immich photo backend plugin for PostcardsRust.
//!
//! Connects to a self-hosted Immich instance, synchronizes photos from a specified album,
//! provides the oldest photo for postcard creation, and removes sent photos from the album
//! so that sending can be automated without resending duplicate postcards.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use postcards_rust_plugin_base::{AlbumSummary, ICommand};
use serde::{Deserialize, Serialize};

/// Normalize Immich URL by removing trailing slashes and `/api` suffix.
pub fn normalize_url(url: &str) -> String {
    let mut s = url.trim().trim_end_matches('/').to_string();
    if s.ends_with("/api") {
        s.truncate(s.len() - 4);
        s = s.trim_end_matches('/').to_string();
    }
    s
}

/// Sanitize filename for safe local disk storage.
fn safe_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if cleaned.is_empty() {
        "photo.jpg".to_string()
    } else {
        cleaned
    }
}

/// Check if filename has an extension typically requiring conversion or preview download.
pub fn is_heic_or_raw_ext(filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    lower.ends_with(".heic")
        || lower.ends_with(".heif")
        || lower.ends_with(".heifs")
        || lower.ends_with(".hif")
        || lower.ends_with(".dng")
        || lower.ends_with(".cr2")
        || lower.ends_with(".cr3")
        || lower.ends_with(".nef")
        || lower.ends_with(".arw")
        || lower.ends_with(".raf")
}

/// Immich configuration options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImmichConfig {
    /// Immich instance base URL (e.g. `http://localhost:2283` or `https://photos.example.com`).
    pub instance_url: String,

    /// Immich API key.
    pub api_key: String,

    /// Album name or album UUID.
    pub album: String,

    /// Local cache directory for downloaded photos.
    pub media_folder: PathBuf,

    /// Whether to accept invalid/self-signed SSL certificates.
    #[serde(default)]
    pub insecure_tls: bool,
}

impl Default for ImmichConfig {
    fn default() -> Self {
        let default_media = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".postcards_rust")
            .join("immich_photos");

        Self {
            instance_url: String::new(),
            api_key: String::new(),
            album: String::new(),
            media_folder: default_media,
            insecure_tls: false,
        }
    }
}

impl ImmichConfig {
    /// Load configuration from environment variables.
    pub fn from_env() -> Result<Self> {
        let instance_url = std::env::var("IMMICH_INSTANCE_URL")
            .or_else(|_| std::env::var("IMMICH_URL"))
            .map_err(|_| anyhow::anyhow!("IMMICH_INSTANCE_URL (or IMMICH_URL) is not set"))?;

        let api_key = std::env::var("IMMICH_API_KEY")
            .map_err(|_| anyhow::anyhow!("IMMICH_API_KEY is not set"))?;

        let album = std::env::var("IMMICH_ALBUM")
            .or_else(|_| std::env::var("IMMICH_ALBUM_NAME"))
            .or_else(|_| std::env::var("IMMICH_ALBUM_ID"))
            .map_err(|_| anyhow::anyhow!("IMMICH_ALBUM is not set"))?;

        let media_folder = std::env::var("IMMICH_MEDIA_FOLDER")
            .or_else(|_| std::env::var("PCDNCLI_MEDIAFOLDERPATH"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".postcards_rust")
                    .join("immich_photos")
            });

        let insecure_tls = std::env::var("IMMICH_INSECURE_TLS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes"))
            .unwrap_or(false);

        Ok(Self {
            instance_url: normalize_url(&instance_url),
            api_key,
            album,
            media_folder,
            insecure_tls,
        })
    }
}

/// DTO for Immich `/api/server/version` endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerVersionDto {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

/// DTO for Immich `/api/users/me` endpoint.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserDto {
    pub id: String,
    pub email: String,
    #[serde(default)]
    pub name: String,
}

/// DTO for Immich `/api/albums` and `/api/albums/{id}` endpoints.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumDto {
    pub id: String,
    pub album_name: String,
    #[serde(default)]
    pub asset_count: usize,
    #[serde(default)]
    pub assets: Vec<AssetDto>,
}

/// DTO for Immich asset items.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetDto {
    pub id: String,
    #[serde(default)]
    pub original_file_name: String,
    #[serde(rename = "type", default)]
    pub asset_type: String,
    #[serde(default)]
    pub file_created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub local_date_time: Option<DateTime<Utc>>,
}

/// DTO for Immich `/api/search/metadata` response.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchMetadataResponse {
    #[serde(default)]
    assets: SearchAssetsContainer,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SearchAssetsContainer {
    #[serde(default)]
    items: Vec<AssetDto>,
    #[serde(default)]
    #[allow(dead_code)]
    total: usize,
}

/// Bulk IDs payload for `DELETE /api/albums/{id}/assets`.
#[derive(Debug, Serialize)]
struct BulkIdsPayload<'a> {
    ids: &'a [&'a str],
}

/// On-disk manifest for tracking cached album assets.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CacheManifest {
    pub album_id: String,
    pub album_name: String,
    pub items: Vec<CachedAssetItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedAssetItem {
    pub asset_id: String,
    pub original_file_name: String,
    pub local_file_name: String,
    pub created_at: Option<DateTime<Utc>>,
}

/// Immich Photo Backend Plugin Command.
pub struct ImmichCommand {
    config: ImmichConfig,
    http: reqwest::Client,
    resolved_album_id: Option<String>,
    resolved_album_name: Option<String>,
    server_version: Option<String>,
}

impl ImmichCommand {
    /// Create a new Immich command plugin instance.
    pub fn new(mut config: ImmichConfig) -> Result<Self> {
        config.instance_url = normalize_url(&config.instance_url);
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(config.insecure_tls)
            .user_agent("PostcardsRust/0.1 (+ImmichPlugin)")
            .build()
            .context("Failed to build HTTP client for Immich")?;

        Ok(Self {
            config,
            http,
            resolved_album_id: None,
            resolved_album_name: None,
            server_version: None,
        })
    }

    /// Construct from environment variables.
    pub fn from_env() -> Result<Self> {
        Self::new(ImmichConfig::from_env()?)
    }

    /// Get reference to configuration.
    pub fn config(&self) -> &ImmichConfig {
        &self.config
    }

    /// Path to the local manifest file inside `media_folder`.
    fn manifest_path(&self) -> PathBuf {
        self.config.media_folder.join(".immich_manifest.json")
    }

    /// Load on-disk manifest.
    fn load_manifest(&self) -> CacheManifest {
        let path = self.manifest_path();
        if let Ok(data) = std::fs::read_to_string(&path) {
            if let Ok(manifest) = serde_json::from_str::<CacheManifest>(&data) {
                return manifest;
            }
        }
        CacheManifest::default()
    }

    /// Save on-disk manifest.
    fn save_manifest(&self, manifest: &CacheManifest) -> Result<()> {
        if !self.config.media_folder.exists() {
            std::fs::create_dir_all(&self.config.media_folder)?;
        }
        let data = serde_json::to_string_pretty(manifest)?;
        std::fs::write(self.manifest_path(), data)?;
        Ok(())
    }

    /// Ping Immich and get version string.
    pub async fn check_server_version(&mut self) -> Result<String> {
        let url = format!("{}/api/server/version", self.config.instance_url);
        let resp = self.http.get(&url).send().await;
        match resp {
            Ok(r) if r.status().is_success() => {
                if let Ok(v) = r.json::<ServerVersionDto>().await {
                    let ver = format!("{}.{}.{}", v.major, v.minor, v.patch);
                    self.server_version = Some(ver.clone());
                    return Ok(ver);
                }
            }
            Ok(r) => {
                tracing::debug!("server/version returned status {}", r.status());
            }
            Err(e) => {
                tracing::debug!("server/version request error: {e}");
            }
        }
        Ok("unknown".to_string())
    }

    /// Verify credentials by calling `/api/users/me`.
    pub async fn verify_credentials(&self) -> Result<UserDto> {
        let url = format!("{}/api/users/me", self.config.instance_url);
        let resp = self
            .http
            .get(&url)
            .header("x-api-key", &self.config.api_key)
            .header("Accept", "application/json")
            .send()
            .await
            .context("Failed to connect to Immich server")?;

        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            bail!("Immich authentication failed: API key invalid or unauthorized (HTTP {status})");
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Immich /api/users/me failed: HTTP {status} - {body}");
        }

        let user: UserDto = resp
            .json()
            .await
            .context("Failed to parse Immich user profile response")?;
        Ok(user)
    }

    /// List all albums accessible by the user.
    pub async fn fetch_all_albums(&self) -> Result<Vec<AlbumDto>> {
        let url = format!("{}/api/albums", self.config.instance_url);
        let resp = self
            .http
            .get(&url)
            .header("x-api-key", &self.config.api_key)
            .header("Accept", "application/json")
            .send()
            .await
            .context("Failed to list Immich albums")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to list Immich albums: HTTP {status} - {body}");
        }

        let albums: Vec<AlbumDto> = resp
            .json()
            .await
            .context("Failed to parse Immich albums list")?;
        Ok(albums)
    }

    /// Resolve configured album name or UUID to (album_id, album_name).
    pub async fn resolve_album(&mut self) -> Result<(String, String)> {
        if let (Some(id), Some(name)) = (&self.resolved_album_id, &self.resolved_album_name) {
            return Ok((id.clone(), name.clone()));
        }

        let album_input = self.config.album.trim();
        let albums = self.fetch_all_albums().await?;

        // 1. Direct match by UUID
        if let Some(a) = albums.iter().find(|a| a.id == album_input) {
            self.resolved_album_id = Some(a.id.clone());
            self.resolved_album_name = Some(a.album_name.clone());
            return Ok((a.id.clone(), a.album_name.clone()));
        }

        // 2. Direct match by exact name
        if let Some(a) = albums.iter().find(|a| a.album_name == album_input) {
            self.resolved_album_id = Some(a.id.clone());
            self.resolved_album_name = Some(a.album_name.clone());
            return Ok((a.id.clone(), a.album_name.clone()));
        }

        // 3. Case-insensitive match by name
        if let Some(a) = albums
            .iter()
            .find(|a| a.album_name.eq_ignore_ascii_case(album_input))
        {
            self.resolved_album_id = Some(a.id.clone());
            self.resolved_album_name = Some(a.album_name.clone());
            return Ok((a.id.clone(), a.album_name.clone()));
        }

        let available: Vec<String> = albums.iter().map(|a| format!("\"{}\"", a.album_name)).collect();
        bail!(
            "Album \"{}\" not found in Immich. Available albums: [{}]",
            album_input,
            available.join(", ")
        );
    }

    /// Fetch image assets in an album.
    /// Tries `POST /api/search/metadata` first (modern Immich),
    /// falls back to `GET /api/albums/{id}` (older Immich).
    pub async fn fetch_album_assets(&self, album_id: &str) -> Result<Vec<AssetDto>> {
        // Modern Immich: POST /api/search/metadata with albumIds
        let search_url = format!("{}/api/search/metadata", self.config.instance_url);
        let search_body = serde_json::json!({
            "albumIds": [album_id],
            "type": "IMAGE"
        });

        let search_resp = self
            .http
            .post(&search_url)
            .header("x-api-key", &self.config.api_key)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&search_body)
            .send()
            .await;

        if let Ok(resp) = search_resp {
            if resp.status().is_success() {
                if let Ok(data) = resp.json::<SearchMetadataResponse>().await {
                    if !data.assets.items.is_empty() {
                        return Ok(data.assets.items);
                    }
                }
            }
        }

        // Fallback: GET /api/albums/{album_id}
        let album_url = format!("{}/api/albums/{}", self.config.instance_url, album_id);
        let resp = self
            .http
            .get(&album_url)
            .header("x-api-key", &self.config.api_key)
            .header("Accept", "application/json")
            .send()
            .await
            .context("Failed to get album details from Immich")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Immich GET /api/albums/{album_id} failed: HTTP {status} - {body}");
        }

        let album_dto: AlbumDto = resp
            .json()
            .await
            .context("Failed to parse album response")?;

        // Filter for images only (exclude videos)
        let images: Vec<AssetDto> = album_dto
            .assets
            .into_iter()
            .filter(|a| a.asset_type.is_empty() || a.asset_type.eq_ignore_ascii_case("IMAGE"))
            .collect();

        Ok(images)
    }

    /// Download asset original image bytes.
    pub async fn download_asset(&self, asset_id: &str) -> Result<Vec<u8>> {
        let url = format!("{}/api/assets/{}/original", self.config.instance_url, asset_id);
        let resp = self
            .http
            .get(&url)
            .header("x-api-key", &self.config.api_key)
            .send()
            .await
            .with_context(|| format!("Failed to download asset {asset_id} from Immich"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to download asset {asset_id}: HTTP {status} - {body}");
        }

        let bytes = resp.bytes().await?.to_vec();
        if bytes.is_empty() {
            bail!("Downloaded asset {asset_id} has 0 bytes");
        }
        Ok(bytes)
    }

    /// Download preview thumbnail image bytes (JPEG/WebP generated by Immich).
    pub async fn download_preview(&self, asset_id: &str) -> Result<Vec<u8>> {
        let url = format!("{}/api/assets/{}/thumbnail?size=preview", self.config.instance_url, asset_id);
        let resp = self
            .http
            .get(&url)
            .header("x-api-key", &self.config.api_key)
            .send()
            .await
            .with_context(|| format!("Failed to download preview for asset {asset_id} from Immich"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to download preview for asset {asset_id}: HTTP {status} - {body}");
        }

        let bytes = resp.bytes().await?.to_vec();
        if bytes.is_empty() {
            bail!("Downloaded preview for asset {asset_id} has 0 bytes");
        }
        Ok(bytes)
    }

    /// Download asset photo, automatically utilizing Immich's high-res JPEG/WebP preview
    /// if the original file is HEIC/HEIF or cannot be decoded directly.
    /// Returns (bytes, extension).
    pub async fn download_photo(&self, asset_id: &str, original_filename: &str) -> Result<(Vec<u8>, String)> {
        if is_heic_or_raw_ext(original_filename) {
            tracing::info!(
                asset_id = %asset_id,
                filename = %original_filename,
                "Asset is HEIC/RAW, downloading Immich high-res JPEG/WebP preview..."
            );
            match self.download_preview(asset_id).await {
                Ok(bytes) => {
                    let ext = match image::guess_format(&bytes) {
                        Ok(image::ImageFormat::WebP) => "webp",
                        _ => "jpg",
                    };
                    return Ok((bytes, ext.to_string()));
                }
                Err(e) => {
                    tracing::warn!("Failed to fetch preview ({e}), falling back to original...");
                }
            }
        }

        // Try downloading original
        match self.download_asset(asset_id).await {
            Ok(bytes) => {
                if image::guess_format(&bytes).is_ok() {
                    let ext = std::path::Path::new(original_filename)
                        .extension()
                        .and_then(|s| s.to_str())
                        .unwrap_or("jpg")
                        .to_string();
                    return Ok((bytes, ext));
                }

                // Original bytes couldn't be decoded
                tracing::info!(
                    asset_id = %asset_id,
                    filename = %original_filename,
                    "Original image format not directly decodable, downloading Immich preview..."
                );
                let preview_bytes = self.download_preview(asset_id).await?;
                let ext = match image::guess_format(&preview_bytes) {
                    Ok(image::ImageFormat::WebP) => "webp",
                    _ => "jpg",
                };
                Ok((preview_bytes, ext.to_string()))
            }
            Err(e) => {
                tracing::warn!("Failed to download original asset {asset_id} ({e}), falling back to preview...");
                let preview_bytes = self.download_preview(asset_id).await?;
                Ok((preview_bytes, "jpg".to_string()))
            }
        }
    }

    /// Remove an asset from the specified album (unlinks without deleting from library).
    pub async fn remove_asset_from_album(&self, album_id: &str, asset_id: &str) -> Result<()> {
        let url = format!("{}/api/albums/{}/assets", self.config.instance_url, album_id);
        let payload = BulkIdsPayload { ids: &[asset_id] };

        let resp = self
            .http
            .delete(&url)
            .header("x-api-key", &self.config.api_key)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&payload)
            .send()
            .await
            .with_context(|| format!("Failed to delete asset {asset_id} from album {album_id}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to remove asset {asset_id} from album: HTTP {status} - {body}");
        }

        tracing::info!(
            asset_id = %asset_id,
            album_id = %album_id,
            "Successfully removed asset from Immich album"
        );
        Ok(())
    }
}

#[async_trait]
impl ICommand for ImmichCommand {
    fn name(&self) -> &str {
        "Immich Plugin"
    }

    fn description(&self) -> &str {
        "Syncs photos from an Immich album and unlinks sent cards from the album."
    }

    async fn login(&mut self) -> Result<()> {
        let version = self.check_server_version().await.unwrap_or_else(|_| "unknown".to_string());
        let user = self.verify_credentials().await?;
        let (album_id, album_name) = self.resolve_album().await?;

        tracing::info!(
            server_version = %version,
            user_name = %user.name,
            user_email = %user.email,
            album_name = %album_name,
            album_id = %album_id,
            "Logged into Immich successfully"
        );
        Ok(())
    }

    async fn sync(&mut self) -> Result<()> {
        self.login().await?;
        let (album_id, album_name) = self.resolve_album().await?;

        std::fs::create_dir_all(&self.config.media_folder)
            .with_context(|| format!("Failed to create media folder: {:?}", self.config.media_folder))?;

        let mut remote_assets = self.fetch_album_assets(&album_id).await?;
        // Sort oldest first by file creation date
        remote_assets.sort_by_key(|a| a.file_created_at.or(a.local_date_time).unwrap_or(DateTime::<Utc>::MIN_UTC));

        let mut manifest = self.load_manifest();
        manifest.album_id = album_id.clone();
        manifest.album_name = album_name.clone();

        let remote_ids: std::collections::HashSet<String> = remote_assets.iter().map(|a| a.id.clone()).collect();

        // 1. Prune local assets that are no longer in the remote album
        manifest.items.retain(|item| {
            let still_in_album = remote_ids.contains(&item.asset_id);
            if !still_in_album {
                let local_path = self.config.media_folder.join(&item.local_file_name);
                if local_path.exists() {
                    let _ = std::fs::remove_file(&local_path);
                    tracing::debug!("Removed pruned local photo: {:?}", local_path);
                }
            }
            still_in_album
        });

        // 2. Download any new assets from remote album
        for asset in remote_assets {
            let safe_stem = std::path::Path::new(&asset.original_file_name)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("photo");
            let safe_name = safe_filename(safe_stem);

            // Clean up any existing unsupported/HEIC files for this asset id
            let asset_prefix = format!("{}_", asset.id);
            if let Ok(entries) = std::fs::read_dir(&self.config.media_folder) {
                for entry in entries.flatten() {
                    let fname = entry.file_name().to_string_lossy().to_string();
                    if fname.starts_with(&asset_prefix) {
                        let is_bad = is_heic_or_raw_ext(&fname)
                            || std::fs::read(entry.path())
                                .map(|b| image::guess_format(&b).is_err())
                                .unwrap_or(true);
                        if is_bad {
                            tracing::info!("Removing non-decodable cached file: {}", fname);
                            let _ = std::fs::remove_file(entry.path());
                            manifest.items.retain(|i| i.asset_id != asset.id);
                        }
                    }
                }
            }

            let already_cached = manifest.items.iter().any(|i| i.asset_id == asset.id);
            if already_cached {
                if let Some(item) = manifest.items.iter().find(|i| i.asset_id == asset.id) {
                    let p = self.config.media_folder.join(&item.local_file_name);
                    if p.exists() {
                        continue;
                    }
                }
            }

            tracing::info!(
                asset_id = %asset.id,
                filename = %asset.original_file_name,
                "Downloading photo from Immich album"
            );

            let (bytes, ext) = self.download_photo(&asset.id, &asset.original_file_name).await?;
            let local_file_name = format!("{}_{}.{}", asset.id, safe_name, ext);
            let local_path = self.config.media_folder.join(&local_file_name);

            std::fs::write(&local_path, bytes)?;

            let created_at = asset.file_created_at.or(asset.local_date_time);
            manifest.items.retain(|i| i.asset_id != asset.id);
            manifest.items.push(CachedAssetItem {
                asset_id: asset.id,
                original_file_name: asset.original_file_name,
                local_file_name,
                created_at,
            });
        }

        self.save_manifest(&manifest)?;
        tracing::info!(
            total_cached = manifest.items.len(),
            album = %album_name,
            "Immich album sync completed"
        );
        Ok(())
    }

    async fn get_next_photo(&self) -> Result<PathBuf> {
        let manifest = self.load_manifest();
        if manifest.items.is_empty() {
            // Fallback: check files directly in media_folder
            if self.config.media_folder.exists() {
                let mut entries = Vec::new();
                for entry in std::fs::read_dir(&self.config.media_folder)? {
                    let entry = entry?;
                    let path = entry.path();
                    if path.is_file() && path.file_name().and_then(|n| n.to_str()).map(|n| !n.starts_with('.')).unwrap_or(false) {
                        if let Ok(bytes) = std::fs::read(&path) {
                            if image::guess_format(&bytes).is_ok() {
                                let mtime = entry.metadata()?.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                                entries.push((mtime, path));
                            }
                        }
                    }
                }
                entries.sort_by_key(|(t, _)| *t);
                if let Some((_, p)) = entries.into_iter().next() {
                    return Ok(p);
                }
            }
            bail!(
                "No photos available in Immich album \"{}\" (media folder: {:?})",
                self.config.album,
                self.config.media_folder
            );
        }

        // Return the first photo in the manifest (sorted oldest first) that exists on disk and is decodable
        for item in &manifest.items {
            let path = self.config.media_folder.join(&item.local_file_name);
            if path.exists() {
                if let Ok(bytes) = std::fs::read(&path) {
                    if image::guess_format(&bytes).is_ok() {
                        return Ok(path);
                    } else {
                        tracing::warn!("Cached photo {} cannot be decoded by image crate; skipping", path.display());
                    }
                }
            }
        }

        bail!(
            "No valid cached photos currently exist on disk in {:?}",
            self.config.media_folder
        );
    }

    async fn on_photo_sent(&mut self, path_to_photo: &Path) -> Result<()> {
        let file_name = path_to_photo
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();

        let mut manifest = self.load_manifest();
        let (album_id, album_name) = self.resolve_album().await?;

        // Find matching item in manifest
        let found = manifest
            .items
            .iter()
            .find(|i| i.local_file_name == file_name || path_to_photo.ends_with(&i.local_file_name))
            .cloned();

        let asset_id = match found {
            Some(item) => item.asset_id,
            None => {
                // Try extracting asset ID from prefix: "{asset_id}_{filename}"
                if let Some((prefix, _)) = file_name.split_once('_') {
                    prefix.to_string()
                } else {
                    file_name.clone()
                }
            }
        };

        // 1. Remove asset from remote Immich album
        self.remove_asset_from_album(&album_id, &asset_id).await?;

        // 2. Delete local cached file
        if path_to_photo.exists() {
            let _ = std::fs::remove_file(path_to_photo);
        }

        // Clean up any remaining files for this asset ID (e.g. old .HEIC leftover)
        let asset_prefix = format!("{}_", asset_id);
        if let Ok(entries) = std::fs::read_dir(&self.config.media_folder) {
            for entry in entries.flatten() {
                let fname = entry.file_name().to_string_lossy().to_string();
                if fname.starts_with(&asset_prefix) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }

        // 3. Remove from manifest and save
        manifest.items.retain(|i| i.asset_id != asset_id);
        self.save_manifest(&manifest)?;

        tracing::info!(
            asset_id = %asset_id,
            album = %album_name,
            "Photo removed from Immich album and local cache pruned"
        );
        Ok(())
    }

    async fn delete_cached_photo(&self, path_to_photo: &Path) -> Result<()> {
        if path_to_photo.exists() {
            std::fs::remove_file(path_to_photo)?;
        }
        let file_name = path_to_photo
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();

        let mut manifest = self.load_manifest();
        manifest.items.retain(|i| i.local_file_name != file_name);
        let _ = self.save_manifest(&manifest);
        Ok(())
    }

    async fn list_albums(&self) -> Result<Vec<AlbumSummary>> {
        let albums = self.fetch_all_albums().await?;
        let summaries = albums
            .into_iter()
            .map(|a| AlbumSummary {
                id: a.id,
                name: a.album_name,
                asset_count: a.asset_count,
            })
            .collect();
        Ok(summaries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_url() {
        assert_eq!(normalize_url("http://localhost:2283"), "http://localhost:2283");
        assert_eq!(normalize_url("http://localhost:2283/"), "http://localhost:2283");
        assert_eq!(normalize_url("http://localhost:2283/api"), "http://localhost:2283");
        assert_eq!(normalize_url("http://localhost:2283/api/"), "http://localhost:2283");
        assert_eq!(normalize_url("https://photos.example.com/"), "https://photos.example.com");
    }

    #[test]
    fn test_safe_filename() {
        assert_eq!(safe_filename("vacation 2024.jpg"), "vacation_2024.jpg");
        assert_eq!(safe_filename("../escape/test.png"), ".._escape_test.png");
        assert_eq!(safe_filename(""), "photo.jpg");
    }

    #[test]
    fn test_manifest_serialization() {
        let manifest = CacheManifest {
            album_id: "test-album-123".to_string(),
            album_name: "Postcards".to_string(),
            items: vec![CachedAssetItem {
                asset_id: "asset-1".to_string(),
                original_file_name: "photo.jpg".to_string(),
                local_file_name: "asset-1_photo.jpg".to_string(),
                created_at: None,
            }],
        };

        let json = serde_json::to_string(&manifest).unwrap();
        let deserialized: CacheManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.album_id, "test-album-123");
        assert_eq!(deserialized.items.len(), 1);
        assert_eq!(deserialized.items[0].asset_id, "asset-1");
    }

    #[test]
    fn test_bulk_ids_payload_json() {
        let payload = BulkIdsPayload { ids: &["id-1", "id-2"] };
        let json = serde_json::to_string(&payload).unwrap();
        assert_eq!(json, r#"{"ids":["id-1","id-2"]}"#);
    }

    #[tokio::test]
    async fn test_immich_end_to_end_mock_lifecycle() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server_url = format!("http://127.0.0.1:{port}");

        let server = tokio::spawn(async move {
            for _ in 0..10 {
                let (mut socket, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let mut buf = [0u8; 4096];
                let n = match socket.read(&mut buf).await {
                    Ok(n) if n > 0 => n,
                    _ => continue,
                };
                let req = String::from_utf8_lossy(&buf[..n]);

                if req.contains("GET /api/server/version") {
                    let resp = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 32\r\n\r\n{\"major\":1,\"minor\":106,\"patch\":0}";
                    let _ = socket.write_all(resp.as_bytes()).await;
                } else if req.contains("GET /api/users/me") {
                    let body = r#"{"id":"u1","email":"tester@example.com","name":"Tester"}"#;
                    let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", body.len(), body);
                    let _ = socket.write_all(resp.as_bytes()).await;
                } else if req.contains("GET /api/albums") {
                    let body = r#"[{"id":"album-uuid-1","albumName":"PostcardAlbum","assetCount":1,"assets":[]}]"#;
                    let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", body.len(), body);
                    let _ = socket.write_all(resp.as_bytes()).await;
                } else if req.contains("POST /api/search/metadata") {
                    let body = r#"{"assets":{"total":1,"items":[{"id":"asset-uuid-1","originalFileName":"vacation.jpg","type":"IMAGE","fileCreatedAt":"2025-01-01T12:00:00Z"}]}}"#;
                    let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", body.len(), body);
                    let _ = socket.write_all(resp.as_bytes()).await;
                } else if req.contains("GET /api/assets/asset-uuid-1/original") || req.contains("GET /api/assets/asset-uuid-1/thumbnail") {
                    let mut jpeg_bytes = Vec::new();
                    let dummy = image::DynamicImage::new(10, 10, image::ColorType::Rgb8);
                    dummy.write_to(&mut std::io::Cursor::new(&mut jpeg_bytes), image::ImageFormat::Jpeg).unwrap();
                    let header = format!("HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", jpeg_bytes.len());
                    let _ = socket.write_all(header.as_bytes()).await;
                    let _ = socket.write_all(&jpeg_bytes).await;
                } else if req.contains("DELETE /api/albums/album-uuid-1/assets") {
                    let body = r#"[{"id":"asset-uuid-1","success":true}]"#;
                    let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", body.len(), body);
                    let _ = socket.write_all(resp.as_bytes()).await;
                }
            }
        });

        let temp_dir = std::env::temp_dir().join(format!(
            "immich_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config = ImmichConfig {
            instance_url: server_url,
            api_key: "mock-key".to_string(),
            album: "PostcardAlbum".to_string(),
            media_folder: temp_dir.clone(),
            insecure_tls: false,
        };

        let mut plugin = ImmichCommand::new(config).unwrap();
        plugin.login().await.unwrap();
        plugin.sync().await.unwrap();

        let photo = plugin.get_next_photo().await.unwrap();
        assert!(photo.exists());
        assert!(photo.to_string_lossy().contains("asset-uuid-1_vacation.jpg"));

        plugin.on_photo_sent(&photo).await.unwrap();
        assert!(
            !photo.exists(),
            "Photo should have been deleted from local cache after sending"
        );

        server.abort();
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn test_heic_asset_downloads_preview_as_jpeg() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server_url = format!("http://127.0.0.1:{port}");

        let server = tokio::spawn(async move {
            for _ in 0..10 {
                let (mut socket, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let mut buf = [0u8; 4096];
                let n = match socket.read(&mut buf).await {
                    Ok(n) if n > 0 => n,
                    _ => continue,
                };
                let req = String::from_utf8_lossy(&buf[..n]);

                if req.contains("GET /api/server/version") {
                    let resp = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 32\r\n\r\n{\"major\":1,\"minor\":106,\"patch\":0}";
                    let _ = socket.write_all(resp.as_bytes()).await;
                } else if req.contains("GET /api/users/me") {
                    let body = r#"{"id":"u1","email":"tester@example.com","name":"Tester"}"#;
                    let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", body.len(), body);
                    let _ = socket.write_all(resp.as_bytes()).await;
                } else if req.contains("GET /api/albums") {
                    let body = r#"[{"id":"album-uuid-1","albumName":"PostcardAlbum","assetCount":1,"assets":[]}]"#;
                    let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", body.len(), body);
                    let _ = socket.write_all(resp.as_bytes()).await;
                } else if req.contains("POST /api/search/metadata") {
                    let body = r#"{"assets":{"total":1,"items":[{"id":"asset-heic-1","originalFileName":"IMG_3940.HEIC","type":"IMAGE","fileCreatedAt":"2025-01-01T12:00:00Z"}]}}"#;
                    let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", body.len(), body);
                    let _ = socket.write_all(resp.as_bytes()).await;
                } else if req.contains("GET /api/assets/asset-heic-1/thumbnail") {
                    let mut jpeg_bytes = Vec::new();
                    let dummy = image::DynamicImage::new(10, 10, image::ColorType::Rgb8);
                    dummy.write_to(&mut std::io::Cursor::new(&mut jpeg_bytes), image::ImageFormat::Jpeg).unwrap();
                    let header = format!("HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", jpeg_bytes.len());
                    let _ = socket.write_all(header.as_bytes()).await;
                    let _ = socket.write_all(&jpeg_bytes).await;
                } else if req.contains("DELETE /api/albums/album-uuid-1/assets") {
                    let body = r#"[{"id":"asset-heic-1","success":true}]"#;
                    let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", body.len(), body);
                    let _ = socket.write_all(resp.as_bytes()).await;
                }
            }
        });

        let temp_dir = std::env::temp_dir().join(format!(
            "immich_heic_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config = ImmichConfig {
            instance_url: server_url,
            api_key: "mock-key".to_string(),
            album: "PostcardAlbum".to_string(),
            media_folder: temp_dir.clone(),
            insecure_tls: false,
        };

        let mut plugin = ImmichCommand::new(config).unwrap();
        plugin.login().await.unwrap();
        plugin.sync().await.unwrap();

        let photo = plugin.get_next_photo().await.unwrap();
        assert!(photo.exists());
        // Verify it was saved as .jpg (preview fallback) rather than .HEIC
        assert!(photo.to_string_lossy().contains("asset-heic-1_IMG_3940.jpg"));

        plugin.on_photo_sent(&photo).await.unwrap();
        assert!(!photo.exists());

        server.abort();
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
