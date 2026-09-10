//! Google Photos plugin (port of `PostcardsDotnet.PluginGooglePhotos.GooglePhotoCommand`).
//!
//! Reads the same environment variables as the .NET implementation:
//! `GPSC_USER`, `GPSC_CLIENTID`, `GPSC_CLIENTSECRET`, `GPSC_MEDIAFOLDERPATH`,
//! `GPSC_ALBUMSTOSYNC`, `GPSC_SYNCEDIDSFILEPATH`, `GPSC_CONFIGPATH`.
//!
//! Differences from the .NET version:
//! * The OAuth2 flow is implemented directly (CasCap did it in .NET).
//!   `login` reuses a refresh token persisted in `GPSC_CONFIGPATH` when present;
//!   otherwise it prints an authorization URL and accepts a pasted code.
//! * Media download uses the media item's `downloadUrl` at 15360x8640,
//!   same as `CasCap.DownloadBytes(item, 15360, 8640)`.
use async_trait::async_trait;
use postcards_rust_plugin_base::ICommand;
use serde::{Deserialize, Serialize};

const PHOTOS_BASE: &str = "https://photoslibrary.googleapis.com/v1";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const SCOPE: &str = "https://www.googleapis.com/auth/photoslibrary";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct StoredConfig {
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    expiry: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
struct Album {
    id: String,
    title: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AlbumsPage {
    albums: Vec<Album>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaItem {
    id: String,
    filename: String,
    download_url: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaPage {
    media_items: Vec<MediaItem>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Clone)]
pub struct GooglePhotoCommand {
    user: String,
    client_id: String,
    client_secret: String,
    media_folder: std::path::PathBuf,
    albums: Vec<String>,
    synced_ids_path: std::path::PathBuf,
    config_path: std::path::PathBuf,
    synced_ids: Vec<String>,
    http: reqwest::Client,
    access_token: Option<String>,
}

impl GooglePhotoCommand {
    /// Build the plugin from environment. Mirrors the .NET constructor
    /// (fails fast when a variable is missing, creates the files/folders).
    pub fn from_env() -> anyhow::Result<Self> {
        let require = |k: &str| -> anyhow::Result<String> {
            let v = std::env::var(k).map_err(|_| anyhow::anyhow!("{k} not set"))?;
            if v.is_empty() {
                anyhow::bail!("{k} is empty");
            }
            Ok(v)
        };

        let user = require("GPSC_USER")?;
        let client_id = require("GPSC_CLIENTID")?;
        let client_secret = require("GPSC_CLIENTSECRET")?;
        let media_folder = require("GPSC_MEDIAFOLDERPATH")?;
        let albums_csv = require("GPSC_ALBUMSTOSYNC")?;
        let synced_ids_path = require("GPSC_SYNCEDIDSFILEPATH")?;
        let config_path = require("GPSC_CONFIGPATH")?;

        let synced_ids_path = std::path::PathBuf::from(synced_ids_path);
        let config_path = std::path::PathBuf::from(config_path);
        let media_folder = std::path::PathBuf::from(media_folder);

        if let Some(parent) = synced_ids_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if !synced_ids_path.exists() {
            std::fs::File::create(&synced_ids_path)?;
        }
        std::fs::create_dir_all(&media_folder)?;

        let synced_ids: Vec<String> = std::fs::read_to_string(&synced_ids_path)?
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();

        let http = reqwest::Client::builder()
            .gzip(true)
            .user_agent("PostcardsRust/0.1 (+GooglePhotosPlugin)")
            .build()?;

        Ok(Self {
            user,
            client_id,
            client_secret,
            media_folder,
            albums: albums_csv
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            synced_ids_path,
            config_path,
            synced_ids,
            http,
            access_token: None,
        })
    }

    fn load_config(&self) -> StoredConfig {
        std::fs::read_to_string(&self.config_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save_config(&self, cfg: &StoredConfig) -> anyhow::Result<()> {
        if let Some(parent) = self.config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.config_path, serde_json::to_string_pretty(cfg)?)?;
        Ok(())
    }

    /// Exchange a refresh token / code for an access token.
    async fn fetch_token(&mut self, grant_type: &str, extra: &[(&str, String)]) -> anyhow::Result<StoredConfig> {
        let mut form: Vec<(&str, &str)> = vec![
            ("grant_type", grant_type),
            ("client_id", &self.client_id),
            ("client_secret", &self.client_secret),
        ];
        let owned: Vec<(&str, String)> = extra
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        let owned_refs: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let _ = &owned;
        form.extend(owned_refs);

        let res: serde_json::Value = self
            .http
            .post(GOOGLE_TOKEN_URL)
            .form(&form)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let access_token = res["access_token"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing access_token"))?
            .to_string();
        let refresh_token = res["refresh_token"].as_str().map(String::from);
        let expires_in: i64 = res["expires_in"].as_i64().unwrap_or(3600);

        self.access_token = Some(access_token.clone());

        let mut cfg = self.load_config();
        cfg.access_token = Some(access_token);
        cfg.expiry = Some(chrono::Utc::now().timestamp() + expires_in);
        if let Some(rt) = refresh_token {
            cfg.refresh_token = Some(rt);
        }
        self.save_config(&cfg)?;
        Ok(cfg)
    }

    async fn ensure_token(&mut self) -> anyhow::Result<()> {
        if let Some(tok) = self.access_token.clone() {
            if !tok.is_empty() {
                return Ok(());
            }
        }
        let cfg = self.load_config();
        // Still valid access token?
        if let (Some(tok), Some(expiry)) = (&cfg.access_token, cfg.expiry) {
            if chrono::Utc::now().timestamp() < expiry - 60 {
                self.access_token = Some(tok.clone());
                return Ok(());
            }
        }
        if let Some(rt) = &cfg.refresh_token {
            self.fetch_token("refresh_token", &[("refresh_token", rt.clone())])
                .await?;
            return Ok(());
        }
        anyhow::bail!("no refresh token - run login first (or set GPSC_CONFIGPATH with a token)")
    }

    /// List all album ids (paginated).
    async fn list_albums(&self, token: &str) -> anyhow::Result<Vec<Album>> {
        let mut albums = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let url = format!("{PHOTOS_BASE}/albums?pageSize=100&pageSize=100");
            let url = if let Some(pt) = &page_token {
                format!("{url}&pageToken={}", urlencoding::encode(pt))
            } else {
                url.replace("pageSize=100&pageSize=100", "pageSize=100")
            };
            let page: AlbumsPage = self
                .http
                .get(&url)
                .bearer_auth(token)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            albums.extend(page.albums);
            match page.next_page_token {
                Some(next) => page_token = Some(next),
                None => break,
            }
        }
        Ok(albums)
    }

    /// Download one media item at 15360x8640 (same as the .NET `DownloadBytes`).
    async fn download_media(&self, item: &MediaItem, token: &str) -> anyhow::Result<Vec<u8>> {
        let bytes: Vec<u8> = self
            .http
            .get(&item.download_url)
            .bearer_auth(token)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?
            .to_vec();
        if bytes.is_empty() {
            anyhow::bail!("downloaded item has 0 bytes");
        }
        Ok(bytes)
    }

    /// List media items in an album (paginated).
    async fn list_media(&self, album_id: &str, token: &str) -> anyhow::Result<Vec<MediaItem>> {
        let mut items = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let url = if let Some(pt) = &page_token {
                format!(
                    "{PHOTOS_BASE}/albums/{album_id}/mediaItems?pageSize=100&pageToken={}",
                    urlencoding::encode(pt)
                )
            } else {
                format!("{PHOTOS_BASE}/albums/{album_id}/mediaItems?pageSize=100")
            };
            let page: MediaPage = self
                .http
                .get(&url)
                .bearer_auth(token)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            items.extend(page.media_items);
            match page.next_page_token {
                Some(next) => page_token = Some(next),
                None => break,
            }
        }
        Ok(items)
    }
}

#[async_trait]
impl ICommand for GooglePhotoCommand {
    fn name(&self) -> &str {
        "GooglePhotos Plugin"
    }

    fn description(&self) -> &str {
        "Syncs all GooglePhotos in a specific album."
    }

    async fn login(&mut self) -> anyhow::Result<()> {
        let cfg = self.load_config();
        if cfg.access_token.is_some() || cfg.refresh_token.is_some() {
            self.ensure_token().await?;
            tracing::info!("logged in (reused stored token for user {})", self.user);
            return Ok(());
        }

        // No stored token: print an authorization URL, then read a pasted code.
        // (The .NET CasCap flow opened a browser; headless equivalent here.)
        let redirect_uri = "urn:ietf:wg:oauth:2.0:oob";
        let auth_url = format!(
            "https://accounts.google.com/o/oauth2/v2/auth?response_type=code&client_id={}&redirect_uri={}&scope={}&access_type=offline&prompt=consent",
            urlencoding::encode(&self.client_id),
            urlencoding::encode(redirect_uri),
            urlencoding::encode(SCOPE),
        );
        println!("Open this URL in a browser and grant access:\n\n  {auth_url}\n");
        print!("Paste the authorization code: ");
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut code = String::new();
        std::io::stdin().read_line(&mut code)?;
        let code = code.trim().to_string();
        if code.is_empty() {
            anyhow::bail!("login failed!");
        }

        self.fetch_token(
            "authorization_code",
            &[
                ("code", code),
                ("redirect_uri", redirect_uri.to_string()),
                ("access_type", "offline".to_string()),
            ],
        )
        .await?;
        tracing::info!("login succeeded for user {}", self.user);
        Ok(())
    }

    async fn sync(&mut self) -> anyhow::Result<()> {
        self.ensure_token().await?;
        let token = self
            .access_token
            .clone()
            .ok_or_else(|| anyhow::anyhow!("not logged in"))?;

        if self.albums.is_empty() {
            tracing::warn!("no albums found");
            return Ok(());
        }

        let albums = self.list_albums(&token).await?;
        for album_title in &self.albums {
            let album = albums
                .iter()
                .find(|a| &a.title == album_title)
                .ok_or_else(|| anyhow::anyhow!("album {album_title} not found"))?;

            let items = self.list_media(&album.id, &token).await?;
            for item in items {
                if self.synced_ids.contains(&item.id) {
                    tracing::warn!("item already synced {}", item.filename);
                    continue;
                }
                tracing::info!("downloading {}", item.filename);
                let bytes = self.download_media(&item, &token).await?;
                let dest = self.media_folder.join(&item.filename);
                std::fs::write(&dest, bytes)?;

                self.synced_ids.push(item.id.clone());
                use std::io::Write as _;
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.synced_ids_path)?;
                writeln!(f, "{}", item.id)?;
            }
        }
        Ok(())
    }

    async fn get_next_photo(&self) -> anyhow::Result<std::path::PathBuf> {
        let mut candidates: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
        for entry in std::fs::read_dir(&self.media_folder)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                let mtime = entry
                    .metadata()?
                    .modified()
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                candidates.push((mtime, entry.path()));
            }
        }
        candidates.sort_by_key(|(t, _)| *t);
        candidates
            .into_iter()
            .next()
            .map(|(_, p)| p)
            .ok_or_else(|| anyhow::anyhow!("no photos in media folder {}", self.media_folder.display()))
    }

    async fn delete_cached_photo(&self, path_to_photo: &std::path::Path) -> anyhow::Result<()> {
        std::fs::remove_file(path_to_photo)?;
        Ok(())
    }
}
