//! Swiss Postcard Creator API facade (port of `PostcardsDotnet.API.SwissPostcardCreatorApi`).
//!
//! Combines a token service (SwissId login) with the PCC REST API.
pub use postcards_rust_core::types::{
    Balance, PostcardCreatorQuota, PostcardCreatorUser, RecipientAddress, SenderAddress,
};
use postcards_rust_core::types::SwissPostcardCreatorApi as CoreApi;
use postcards_rust_core::swissid::{SwissIdLoginService, TokenService as _};
use postcards_rust_core::Token;

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
        self.token = Some(token);
        Ok(())
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
        self.token = Some(token);
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
}
