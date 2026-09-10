//! Shared record types mirroring the PCC REST API payloads.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::USER_AGENT;

/// OAuth token pair (SwissId / PCC web).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in_seconds: u64,
    pub expires_at: DateTime<Utc>,
}

/// Sender address (camelCase JSON, as the PCC API expects).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SenderAddress {
    pub first_name: String,
    pub last_name: String,
    pub street: String,
    pub zip: String,
    pub city: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
}

/// Recipient address.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipientAddress {
    pub first_name: String,
    pub last_name: String,
    pub street: String,
    pub zip: String,
    pub city: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company_addon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Quota object returned by `user/quota`.
#[derive(Debug, Clone, Deserialize)]
pub struct PostcardCreatorQuota {
    pub quota: u64,
    pub end: DateTime<Utc>,
    pub retention_days: u64,
    pub available: bool,
    #[serde(default)]
    pub next: Option<DateTime<Utc>>,
}

/// Logged-in user object (`user/current`).
#[derive(Debug, Clone, Deserialize)]
pub struct PostcardCreatorUser {
    #[serde(default)]
    pub company: String,
    #[serde(default)]
    pub first_name: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub street: String,
    #[serde(default)]
    pub zip: String,
    #[serde(default)]
    pub city: String,
}

/// Account balance (`billingOnline/accountSaldo`).
#[derive(Debug, Clone, Deserialize)]
pub struct Balance {
    #[serde(default)]
    pub forecast_saldo: Option<f64>,
}

/// Card upload payload.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CardUpload<'a> {
    lang: &'static str,
    paid: Option<bool>,
    recipient: &'a RecipientAddress,
    sender: &'a SenderAddress,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    text_image: Option<String>,
    image: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    stamp: Option<String>,
}

/// Swiss Postcard Creator REST API.
#[derive(Debug, Clone)]
pub struct SwissPostcardCreatorApi {
    client: reqwest::Client,
    access_token: String,
    token: Option<Token>,
    sender: Option<SenderAddress>,
    recipient: Option<RecipientAddress>,
}

impl Default for SwissPostcardCreatorApi {
    fn default() -> Self {
        Self::new()
    }
}

impl SwissPostcardCreatorApi {
    /// Create a new API client (not logged in yet).
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .expect("http client");
        Self {
            client,
            access_token: String::new(),
            token: None,
            sender: None,
            recipient: None,
        }
    }

    /// Set the access token (e.g. after login/refresh).
    pub fn set_access_token(&mut self, access_token: String) {
        self.access_token = access_token;
    }

    fn bearer(&self) -> reqwest::header::HeaderValue {
        reqwest::header::HeaderValue::from_str(&format!("Bearer {}", self.access_token))
            .expect("valid header value")
    }

    /// Login with SwissId credentials.
    pub async fn login(&mut self, username: &str, password: &str) -> anyhow::Result<()> {
        use super::swissid::TokenService as _;
        let svc = super::swissid::SwissIdLoginService::new();
        let token = svc
            .get_token(username, password)
            .await
            .map_err(|e| anyhow::anyhow!("login failed: {e}"))?;
        self.access_token = token.access_token.clone();
        self.token = Some(token);
        Ok(())
    }

    /// Refresh the current token.
    pub async fn refresh_token(&mut self) -> anyhow::Result<()> {
        let refresh = self
            .token
            .as_ref()
            .map(|t| t.refresh_token.clone())
            .ok_or_else(|| anyhow::anyhow!("no valid token, can't refresh"))?;
        use super::swissid::TokenService as _;
        let svc = super::swissid::SwissIdLoginService::new();
        let token = svc
            .refresh_token(&refresh)
            .await
            .map_err(|e| anyhow::anyhow!("refresh failed: {e}"))?;
        self.access_token = token.access_token.clone();
        self.token = Some(token);
        Ok(())
    }

    /// When the current token expires.
    pub fn token_expires_at(&self) -> Option<DateTime<Utc>> {
        self.token.as_ref().map(|t| t.expires_at)
    }

    pub fn set_sender(&mut self, sender: SenderAddress) {
        self.sender = Some(sender);
    }

    pub fn set_recipient(&mut self, recipient: RecipientAddress) {
        self.recipient = Some(recipient);
    }

    /// Get current quota.
    pub async fn get_quota(&self) -> anyhow::Result<PostcardCreatorQuota> {
        let res: serde_json::Value = self
            .client
            .get(format!("{}/user/quota", super::PCC_API_BASE))
            .header(reqwest::header::AUTHORIZATION, self.bearer())
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        serde_json::from_value(res["model"].clone())
            .map_err(|e| anyhow::anyhow!("invalid json: {e}"))
    }

    /// Get logged-in user information.
    pub async fn get_user_information(&self) -> anyhow::Result<PostcardCreatorUser> {
        let res: serde_json::Value = self
            .client
            .get(format!("{}/user/current", super::PCC_API_BASE))
            .header(reqwest::header::AUTHORIZATION, self.bearer())
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        serde_json::from_value(res["model"].clone())
            .map_err(|e| anyhow::anyhow!("invalid json: {e}"))
    }

    /// Get account balance.
    pub async fn get_account_balance(&self) -> anyhow::Result<Balance> {
        let res: serde_json::Value = self
            .client
            .get(format!("{}/billingOnline/accountSaldo", super::PCC_API_BASE))
            .header(reqwest::header::AUTHORIZATION, self.bearer())
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        serde_json::from_value(res["model"].clone())
            .map_err(|e| anyhow::anyhow!("invalid json: {e}"))
    }

    /// Check if a free card is available.
    pub async fn free_card_available(&self) -> anyhow::Result<bool> {
        Ok(self.get_quota().await?.available)
    }

    /// Next date a free card becomes available.
    pub async fn next_free_card_available_at(&self) -> anyhow::Result<Option<DateTime<Utc>>> {
        Ok(self.get_quota().await?.next)
    }

    /// Send a postcard: scales the image, uploads it. Returns true on success.
    pub async fn send_postcard(&self, image: &[u8], message: Option<&str>) -> anyhow::Result<bool> {
        let recipient = self
            .recipient
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no recipient set"))?;
        let sender = self
            .sender
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no sender set"))?;
        let image_b64 = super::scale_and_convert_to_base64(image)?;

        let payload = CardUpload {
            lang: "en",
            paid: Some(false),
            recipient,
            sender,
            text: message.unwrap_or_default().to_string(),
            text_image: None,
            image: image_b64,
            stamp: None,
        };

        let res = self
            .client
            .post(format!("{}/card/upload", super::PCC_API_BASE))
            .header(reqwest::header::AUTHORIZATION, self.bearer())
            .json(&payload)
            .send()
            .await?;
        Ok(res.status().is_success())
    }
}
