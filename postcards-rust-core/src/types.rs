//! Shared record types mirroring the PCC REST API payloads.
use base64::Engine;
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

/// Sender address (exact backend JSON schema: firstname, lastname, street, zip, city, company).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SenderAddress {
    #[serde(rename = "firstname")]
    pub first_name: String,
    #[serde(rename = "lastname")]
    pub last_name: String,
    pub street: String,
    pub zip: String,
    pub city: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
}

fn default_country() -> String {
    "SWITZERLAND".to_string()
}

/// Recipient address (exact backend JSON schema).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecipientAddress {
    #[serde(rename = "firstname")]
    pub first_name: String,
    #[serde(rename = "lastname")]
    pub last_name: String,
    pub street: String,
    pub zip: String,
    pub city: String,
    #[serde(default = "default_country")]
    pub country: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
    #[serde(rename = "companyAddon", skip_serializing_if = "Option::is_none")]
    pub company_addon: Option<String>,
    #[serde(rename = "addressAddOn", skip_serializing_if = "Option::is_none")]
    pub address_addon: Option<String>,
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
    #[serde(default, rename = "firstname")]
    pub first_name: String,
    #[serde(default, rename = "lastname")]
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

/// Card upload payload matching POST /card/uploads.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CardUpload<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    discount_code: Option<String>,
    lang: &'static str,
    paid: bool,
    recipients: &'a [RecipientAddress],
    sender: &'a SenderAddress,
    #[serde(skip_serializing_if = "Option::is_none")]
    sending_date: Option<String>,
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

    /// The current access token (empty before login).
    pub fn access_token(&self) -> &str {
        &self.access_token
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
        let resp = self
            .client
            .get(format!("{}{}", super::PCC_API_BASE, "/user/quota"))
            .header(reqwest::header::AUTHORIZATION, self.bearer())
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::ORIGIN, "https://account.post.ch")
            .header("X-App-Version", "4.38.1.0")
            .header("X-Requested-With", "ch.post.it.pcc")
            .send()
            .await?;
        let status = resp.status();
        let body = resp.bytes().await?;
        let txt = String::from_utf8_lossy(&body);
        tracing::debug!(
            status = %status,
            token_prefix = %self.access_token.chars().take(20).collect::<String>(),
            token_len = self.access_token.len(),
            body_preview = %txt.chars().take(400).collect::<String>(),
            "quota response"
        );
        std::fs::write("/tmp/e2e_quota_resp.txt", format!("status={status}\n{txt}")).ok();
        if !status.is_success() {
            anyhow::bail!("quota failed: {status} {txt}");
        }
        let res: serde_json::Value = serde_json::from_str(&txt)?;
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

        let recipients = [recipient.clone()];
        let payload = CardUpload {
            discount_code: None,
            lang: "en",
            paid: false,
            recipients: &recipients,
            sender,
            sending_date: None,
            text: message.unwrap_or_default().to_string(),
            text_image: None,
            image: image_b64,
            stamp: None,
        };

        let res = self
            .client
            .post(format!("{}/card/uploads", super::PCC_API_BASE))
            .header(reqwest::header::AUTHORIZATION, self.bearer())
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::ORIGIN, "https://account.post.ch")
            .json(&payload)
            .send()
            .await?;
        Ok(res.status().is_success())
    }

    /// Inspect JWT claims in the access token for diagnostics.
    pub fn inspect_jwt_token(&self) -> Option<serde_json::Value> {
        let parts: Vec<&str> = self.access_token.split('.').collect();
        if parts.len() < 2 {
            return None;
        }
        let payload = parts[1];
        let mut padded = payload.to_string();
        while padded.len() % 4 != 0 {
            padded.push('=');
        }
        let decoded = base64::engine::general_purpose::URL_SAFE.decode(padded.as_bytes()).ok()?;
        serde_json::from_slice(&decoded).ok()
    }

    /// Test candidate app-version transports (header, UA, query param) against
    /// `/user/quota` and report which clears the API's `appVersionValidation` guard.
    pub async fn probe_app_version(&self, version: &str) -> anyhow::Result<Vec<(String, String, String)>> {
        let base = format!("{}{}", super::PCC_API_BASE, "/user/quota");
        let mut out = Vec::new();

        if let Some(jwt) = self.inspect_jwt_token() {
            let s = serde_json::to_string(&jwt).unwrap_or_default();
            out.push(("JWT token payload".to_string(), "INFO".to_string(), s));
        }

        // Test other endpoints first with default headers
        for ep in &["/user/current", "/billingOnline/accountSaldo"] {
            let url = format!("{}{}", super::PCC_API_BASE, ep);
            let (label, status, err) = self.try_request(&url, &[], None, &format!("ENDPOINT {ep}")).await;
            out.push((label, status, err));
        }

        let versions = [version, "4.38.1", "4.38.0", "4.8.2.0"];
        let header_names = [
            "X-App-Version",
            "app-version",
            "App-Version",
            "X-AppVersion",
            "appVersion",
            "AppVersion",
            "X-Application-Version",
            "X-Pcc-Version",
            "X-PCC-Version",
            "X-Pcc-App-Version",
            "X-Post-App-Version",
            "X-Client-Version",
            "Client-Version",
            "client-version",
            "X-Version",
            "Version",
            "version",
            "X-App",
            "X-App-Id",
            "X-App-Name",
            "X-Platform",
            "X-OS",
            "X-Device-OS",
            "X-Release-Version",
            "X-Build-Version",
        ];

        // 1. Headers with different version strings
        for v in &versions[..2] {
            for h in &header_names {
                let (label, status, err) = self
                    .try_request(&base, &[(*h, v)], None, &format!("HDR {h}={v}"))
                    .await;
                out.push((label, status, err));
                if out.last().unwrap().1 == "200 OK" {
                    return Ok(out);
                }
            }
        }

        // 2. User-Agent variations
        let user_agents = [
            format!("PostCard/{version} (Linux; Android 12)"),
            format!("PostCard/4.38.1 (Linux; Android 12)"),
            format!("PostCard/{version}"),
            format!("PostCard/4.38.1"),
            format!("ch.post.it.pcc/{version}"),
            format!("ch.post.it.pcc/4.38.1"),
            format!("PostCardCreator/{version}"),
            format!("PostCardCreator/4.38.1"),
            format!("PostCard {version} (Android)"),
            format!("PostCard/4.38.1.0 (ch.post.it.pcc; Android 12; Pixel 6)"),
            format!("{USER_AGENT} PostCard/{version}"),
            format!("{USER_AGENT} ch.post.it.pcc/{version}"),
            format!("Mozilla/5.0 (Android; Mobile; rv:100.0) Gecko/100.0 Firefox/100.0 PostCard/{version}"),
        ];

        for ua in &user_agents {
            let (label, status, err) = self
                .try_request(&base, &[], Some(ua), &format!("UA {ua}"))
                .await;
            out.push((label, status, err));
            if out.last().unwrap().1 == "200 OK" {
                return Ok(out);
            }
        }

        // 3. Query parameters
        let query_names = [
            "appVersion",
            "app_version",
            "appversion",
            "version",
            "v",
            "clientVersion",
            "client_version",
            "app",
            "ver",
        ];
        for v in &versions[..2] {
            for q in &query_names {
                let url = format!("{base}?{q}={v}");
                let (label, status, err) = self
                    .try_request(&url, &[], None, &format!("QUERY ?{q}={v}"))
                    .await;
                out.push((label, status, err));
                if out.last().unwrap().1 == "200 OK" {
                    return Ok(out);
                }
            }
        }

        // 4. Multi-header combinations (e.g., App-Version + Platform)
        let combos: &[(&[(&str, &str)], &str)] = &[
            (&[("X-App-Version", version), ("X-Platform", "android")], "HDR X-App-Version + X-Platform=android"),
            (&[("App-Version", version), ("X-Platform", "Android")], "HDR App-Version + X-Platform=Android"),
            (&[("X-App-Version", "4.38.1"), ("X-Requested-With", "ch.post.it.pcc")], "HDR X-App-Version=4.38.1 + X-Req-With"),
            (&[("App-Version", "4.38.1"), ("X-Requested-With", "ch.post.it.pcc")], "HDR App-Version=4.38.1 + X-Req-With"),
            (&[("X-AppVersion", "4.38.1"), ("X-App-Name", "ch.post.it.pcc")], "HDR X-AppVersion=4.38.1 + X-App-Name"),
            (&[("Client-Version", "4.38.1"), ("X-Requested-With", "ch.post.it.pcc")], "HDR Client-Version=4.38.1"),
        ];
        for (headers, label) in combos {
            let (l, status, err) = self.try_request(&base, headers, None, label).await;
            out.push((l, status, err));
            if out.last().unwrap().1 == "200 OK" {
                return Ok(out);
            }
        }

        Ok(out)
    }

    async fn try_request(
        &self,
        url: &str,
        extra_headers: &[(&str, &str)],
        custom_ua: Option<&str>,
        label: &str,
    ) -> (String, String, String) {
        let mut req = self
            .client
            .get(url)
            .header(reqwest::header::AUTHORIZATION, self.bearer())
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::ORIGIN, "https://account.post.ch");

        if let Some(ua) = custom_ua {
            req = req.header(reqwest::header::USER_AGENT, ua);
        }
        for (k, v) in extra_headers {
            req = req.header(*k, *v);
        }

        match req.send().await {
            Ok(r) => {
                let status = r.status();
                let body = String::from_utf8_lossy(&r.bytes().await.unwrap_or_default()).to_string();
                let err = if body.contains("appVersion") {
                    "appVersionValidation".to_string()
                } else if body.len() > 100 {
                    format!("{}...", &body[..80])
                } else {
                    body
                };
                (label.to_string(), status.to_string(), err)
            }
            Err(e) => (label.to_string(), "ERR".to_string(), e.to_string()),
        }
    }
}
