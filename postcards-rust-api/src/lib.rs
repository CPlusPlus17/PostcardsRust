//! Swiss Postcard Creator API facade (port of `PostcardsDotnet.API.SwissPostcardCreatorApi`).
//!
//! Combines a token service (SwissId login) with the PCC REST API.
pub use postcards_rust_core::types::{
    Balance, PostcardCreatorQuota, PostcardCreatorUser, RecipientAddress, SenderAddress, Token,
};
use postcards_rust_core::types::SwissPostcardCreatorApi as CoreApi;
use postcards_rust_core::swissid::{SwissIdLoginService, TokenService as _};

/// On-disk token cache so one login (one 2FA) can be reused across many runs.
fn token_cache_path() -> std::path::PathBuf {
    let mut p = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    p.push(".postcards_rust");
    p.push("token.json");
    p
}

fn load_cached_token() -> Option<Token> {
    let path = token_cache_path();
    let s = std::fs::read_to_string(path).ok()?;
    let t: Token = serde_json::from_str(&s).ok()?;
    Some(t)
}

fn save_token(token: &Token) {
    let path = token_cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(s) = serde_json::to_string_pretty(token) {
        let _ = std::fs::write(&path, s);
        let _ = std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600));
    }
}

/// High-level Postcard Creator API with SwissId login.
#[derive(Debug, Default)]
pub struct SwissPostcardCreatorApi {
    token_service: SwissIdLoginService,
    core: CoreApi,
    token: Option<Token>,
}

impl SwissPostcardCreatorApi {
    /// Constructor with the default (SwissId) token service.
    pub fn new() -> Self {
        Self {
            token_service: SwissIdLoginService::new(),
            core: CoreApi::new(),
            token: None,
        }
    }

    /// Try to login and get a token.
    pub async fn login(&mut self, username: &str, password: &str) -> anyhow::Result<()> {
        let token = self.token_service.get_token(username, password).await?;
        self.set_access_token(token.access_token.clone());
        self.token = Some(token.clone());
        save_token(&token);
        Ok(())
    }

    /// Cache-first: use a valid cached token (refreshing if needed), else do a full
    /// login. This is what the CLI calls — one 2FA covers many runs.
    pub async fn ensure_token(&mut self, username: &str, password: &str) -> anyhow::Result<()> {
        if let Some(tok) = self.token_from_cache().await {
            self.set_access_token(tok.access_token.clone());
            tracing::info!(
                "using cached PCC token (expires_at={})",
                self.get_token_expires_at().map(|t| t.to_rfc3339()).unwrap_or_default()
            );
            return Ok(());
        }
        tracing::info!("no usable cached token - doing full SwissId login");
        self.login(username, password).await
    }

    /// Try the on-disk token cache; if expired, attempt a refresh. `None` if the
    /// caller must do a full login.
    async fn token_from_cache(&mut self) -> Option<Token> {
        let tok = load_cached_token()?;
        let now = chrono::Utc::now();
        if now < tok.expires_at - chrono::Duration::minutes(1) {
            self.token = Some(tok.clone());
            return Some(tok);
        }
        match self.token_service.refresh_token(&tok.refresh_token).await {
            Ok(t) => {
                self.token = Some(t.clone());
                save_token(&t);
                Some(t)
            }
            Err(e) => {
                tracing::info!("cached token refresh failed: {e}");
                None
            }
        }
    }

    /// Refresh token after login.
    pub async fn refresh_token(&mut self) -> anyhow::Result<()> {
        let token = self
            .token
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No valid token, can't refresh"))?;
        let token = self
            .token_service
            .refresh_token(&token.refresh_token)
            .await?;
        self.set_access_token(token.access_token.clone());
        self.token = Some(token.clone());
        save_token(&token);
        Ok(())
    }

    fn set_access_token(&mut self, token: String) {
        self.core.set_access_token(token);
    }

    /// Get DateTimeOffset when token expires at.
    pub fn get_token_expires_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.token.as_ref().map(|t| t.expires_at)
    }

    /// Set sender address.
    pub fn set_sender(&mut self, sender: SenderAddress) {
        self.core.set_sender(sender);
    }

    /// Set recipient address.
    pub fn set_recipient(&mut self, recipient: RecipientAddress) {
        self.core.set_recipient(recipient);
    }

    /// Send postcard with image and optional text.
    pub async fn send_postcard(&mut self, image: Vec<u8>, message: Option<String>) -> anyhow::Result<bool> {
        // Refresh when the token is about to expire (mirrors the .NET TODO).
        if let Some(expires_at) = self.get_token_expires_at() {
            if chrono::Utc::now() + chrono::Duration::minutes(2) > expires_at {
                tracing::info!("token about to expire - refreshing");
                self.refresh_token().await?;
            }
        }
        self.core.send_postcard(&image, message.as_deref()).await
    }

    /// Get current quota.
    pub async fn get_quota(&self) -> anyhow::Result<PostcardCreatorQuota> {
        self.core.get_quota().await
    }

    /// Get logged-in user information.
    pub async fn get_user_information(&self) -> anyhow::Result<PostcardCreatorUser> {
        self.core.get_user_information().await
    }

    /// Get account balance from logged-in user.
    pub async fn get_account_balance(&self) -> anyhow::Result<Balance> {
        self.core.get_account_balance().await
    }

    /// Check if a free card is available to send.
    pub async fn free_card_available(&self) -> anyhow::Result<bool> {
        self.core.free_card_available().await
    }

    /// Get date and time when next free card can be sent.
    pub async fn next_free_card_available_at(&self) -> anyhow::Result<Option<chrono::DateTime<chrono::Utc>>> {
        self.core.next_free_card_available_at().await
    }

    /// Probe which app-version transport the PCC API accepts (one login).
    pub async fn probe_app_version(&self, version: &str) -> anyhow::Result<Vec<(String, String, String)>> {
        self.core.probe_app_version(version).await
    }
}
