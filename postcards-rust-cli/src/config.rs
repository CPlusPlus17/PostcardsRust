//! Configuration loading and plugin factory for PostcardsRust CLI.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use postcards_rust_api::{RecipientAddress, SenderAddress, SwissPostcardCreatorApi};
use postcards_rust_plugin_base::ICommand;
use postcards_rust_plugin_google_photos::GooglePhotoCommand;
use postcards_rust_plugin_immich::{ImmichCommand, ImmichConfig};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    /// Preferred photo plugin: "immich" or "google-photos"
    #[serde(default)]
    pub plugin: Option<String>,

    /// Cooldown days between sending cards (default: 7)
    #[serde(default)]
    pub cooldown_days: Option<u32>,

    /// Default text message written on the back of the postcard
    #[serde(default)]
    pub default_message: Option<String>,

    /// Immich plugin configuration
    #[serde(default)]
    pub immich: Option<ImmichConfigFile>,

    /// Sender address
    #[serde(default)]
    pub sender: Option<SenderAddress>,

    /// Recipient address
    #[serde(default)]
    pub recipient: Option<RecipientAddress>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ImmichConfigFile {
    pub instance_url: Option<String>,
    pub api_key: Option<String>,
    pub album: Option<String>,
    pub media_folder: Option<String>,
    pub insecure_tls: Option<bool>,
}

impl AppConfig {
    pub fn default_config_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".postcards_rust")
            .join("config.json")
    }

    /// Load config from custom path or default `~/.postcards_rust/config.json`.
    pub fn load(custom_path: Option<&Path>) -> Self {
        let path = custom_path
            .map(PathBuf::from)
            .unwrap_or_else(Self::default_config_path);

        if let Ok(data) = std::fs::read_to_string(&path) {
            if let Ok(cfg) = serde_json::from_str::<Self>(&data) {
                tracing::debug!("Loaded config from {:?}", path);
                return cfg;
            } else {
                tracing::warn!("Failed to parse config file at {:?}", path);
            }
        }
        Self::default()
    }
}

/// Options passed from command line or environment to instantiate a plugin.
#[derive(Debug, Clone, Default)]
pub struct PluginCliOptions {
    pub plugin: Option<String>,
    pub immich_url: Option<String>,
    pub immich_api_key: Option<String>,
    pub immich_album: Option<String>,
    pub media_folder: Option<PathBuf>,
    pub insecure_tls: bool,
}

/// Factory to build the selected or auto-detected photo backend plugin.
pub fn build_plugin(
    cli: &PluginCliOptions,
    config: &AppConfig,
) -> Result<Box<dyn ICommand>> {
    // Determine plugin type:
    // 1. CLI flag `--plugin`
    // 2. Env `PCD_PLUGIN` or `PCDNCLI_PLUGIN_TYPE`
    // 3. Config file `plugin`
    // 4. Auto-detect from available credentials
    let plugin_name = cli
        .plugin
        .clone()
        .or_else(|| std::env::var("PCD_PLUGIN").ok())
        .or_else(|| std::env::var("PCDNCLI_PLUGIN_TYPE").ok())
        .or_else(|| config.plugin.clone())
        .unwrap_or_else(|| {
            // Auto-detect
            if cli.immich_url.is_some()
                || cli.immich_api_key.is_some()
                || std::env::var("IMMICH_INSTANCE_URL").is_ok()
                || std::env::var("IMMICH_URL").is_ok()
                || std::env::var("IMMICH_API_KEY").is_ok()
                || config.immich.is_some()
            {
                "immich".to_string()
            } else if std::env::var("GPSC_CLIENTID").is_ok() {
                "google-photos".to_string()
            } else {
                "immich".to_string() // Default to Immich
            }
        });

    match plugin_name.to_lowercase().as_str() {
        "immich" => {
            let mut imm_cfg = ImmichConfig::default();

            // Apply from config file
            if let Some(cfg) = &config.immich {
                if let Some(u) = &cfg.instance_url {
                    imm_cfg.instance_url = u.clone();
                }
                if let Some(k) = &cfg.api_key {
                    imm_cfg.api_key = k.clone();
                }
                if let Some(a) = &cfg.album {
                    imm_cfg.album = a.clone();
                }
                if let Some(m) = &cfg.media_folder {
                    imm_cfg.media_folder = PathBuf::from(m);
                }
                if let Some(insec) = cfg.insecure_tls {
                    imm_cfg.insecure_tls = insec;
                }
            }

            // Apply from environment variables
            if let Ok(u) = std::env::var("IMMICH_INSTANCE_URL").or_else(|_| std::env::var("IMMICH_URL")) {
                imm_cfg.instance_url = u;
            }
            if let Ok(k) = std::env::var("IMMICH_API_KEY") {
                imm_cfg.api_key = k;
            }
            if let Ok(a) = std::env::var("IMMICH_ALBUM")
                .or_else(|_| std::env::var("IMMICH_ALBUM_NAME"))
                .or_else(|_| std::env::var("IMMICH_ALBUM_ID"))
            {
                imm_cfg.album = a;
            }
            if let Ok(m) = std::env::var("IMMICH_MEDIA_FOLDER").or_else(|_| std::env::var("PCDNCLI_MEDIAFOLDERPATH")) {
                imm_cfg.media_folder = PathBuf::from(m);
            }
            if let Ok(insec) = std::env::var("IMMICH_INSECURE_TLS") {
                imm_cfg.insecure_tls = insec == "1" || insec.eq_ignore_ascii_case("true") || insec.eq_ignore_ascii_case("yes");
            }

            // Apply from CLI flags
            if let Some(u) = &cli.immich_url {
                imm_cfg.instance_url = u.clone();
            }
            if let Some(k) = &cli.immich_api_key {
                imm_cfg.api_key = k.clone();
            }
            if let Some(a) = &cli.immich_album {
                imm_cfg.album = a.clone();
            }
            if let Some(m) = &cli.media_folder {
                imm_cfg.media_folder = m.clone();
            }
            if cli.insecure_tls {
                imm_cfg.insecure_tls = true;
            }

            // Validate requirements
            if imm_cfg.instance_url.is_empty() {
                bail!(
                    "Immich instance URL is required.\n\
                     Set via --immich-url, IMMICH_INSTANCE_URL env var, or in ~/.postcards_rust/config.json"
                );
            }
            if imm_cfg.api_key.is_empty() {
                bail!(
                    "Immich API key is required.\n\
                     Set via --immich-api-key, IMMICH_API_KEY env var, or in ~/.postcards_rust/config.json"
                );
            }
            if imm_cfg.album.is_empty() {
                bail!(
                    "Immich album is required.\n\
                     Set via --immich-album, IMMICH_ALBUM env var, or in ~/.postcards_rust/config.json"
                );
            }

            let cmd = ImmichCommand::new(imm_cfg)?;
            Ok(Box::new(cmd))
        }
        "google-photos" | "googlephotos" | "google" => {
            let cmd = GooglePhotoCommand::from_env().context(
                "Failed to initialize Google Photos plugin. Ensure GPSC_USER, GPSC_CLIENTID, \
                 GPSC_CLIENTSECRET, GPSC_MEDIAFOLDERPATH, GPSC_ALBUMSTOSYNC, GPSC_SYNCEDIDSFILEPATH, \
                 and GPSC_CONFIGPATH are set."
            )?;
            Ok(Box::new(cmd))
        }
        unknown => {
            bail!("Unknown plugin \"{}\". Supported plugins: \"immich\", \"google-photos\"", unknown);
        }
    }
}

/// Populate SwissPostcardCreatorApi sender and recipient addresses from CLI/env/config.
pub fn apply_addresses(api: &mut SwissPostcardCreatorApi, config: &AppConfig) {
    // Sender address
    let mut sender = SenderAddress::default();
    let mut sender_set = false;

    if let Some(s) = &config.sender {
        sender = s.clone();
        sender_set = true;
    }

    if let Ok(first) = std::env::var("PCD_SENDERFIRSTNAME") {
        sender.first_name = first;
        sender_set = true;
    }
    if let Ok(last) = std::env::var("PCD_SENDERLASTNAME") {
        sender.last_name = last;
        sender_set = true;
    }
    if let Ok(street) = std::env::var("PCD_SENDERSTREET") {
        sender.street = street;
        sender_set = true;
    }
    if let Ok(zip) = std::env::var("PCD_SENDERZIP") {
        sender.zip = zip;
        sender_set = true;
    }
    if let Ok(city) = std::env::var("PCD_SENDERCITY") {
        sender.city = city;
        sender_set = true;
    }
    if let Ok(company) = std::env::var("PCD_SENDERCOMPANY") {
        sender.company = Some(company);
        sender_set = true;
    }

    if sender_set {
        api.set_sender(sender);
    }

    // Recipient address
    let mut recipient = RecipientAddress::default();
    let mut recipient_set = false;

    if let Some(r) = &config.recipient {
        recipient = r.clone();
        recipient_set = true;
    }

    if let Ok(first) = std::env::var("PCD_RECIPIENTFIRSTNAME") {
        recipient.first_name = first;
        recipient_set = true;
    }
    if let Ok(last) = std::env::var("PCD_RECIPIENTLASTNAME") {
        recipient.last_name = last;
        recipient_set = true;
    }
    if let Ok(street) = std::env::var("PCD_RECIPIENTSTREET") {
        recipient.street = street;
        recipient_set = true;
    }
    if let Ok(zip) = std::env::var("PCD_RECIPIENTZIP") {
        recipient.zip = zip;
        recipient_set = true;
    }
    if let Ok(city) = std::env::var("PCD_RECIPIENTCITY") {
        recipient.city = city;
        recipient_set = true;
    }
    if let Ok(country) = std::env::var("PCD_RECIPIENTCOUNTRY") {
        recipient.country = country;
        recipient_set = true;
    }
    if let Ok(company) = std::env::var("PCD_RECIPIENTCOMPANY") {
        recipient.company = Some(company);
        recipient_set = true;
    }

    if recipient_set {
        api.set_recipient(recipient);
    }
}
