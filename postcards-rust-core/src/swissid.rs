//! SwissId login flow (port of `PostcardsDotnet.Services.SwissIdLoginService`).
//!
//! Steps (identical to the .NET implementation):
//! 1.  PCC web OAuth authorization (seed cookies)
//! 2.  Swiss Post IdP login, extract `goto` parameter
//! 3.  SwissId api-login: token/status, welcome-pack, init (authId), basic
//! 4.  Wait for 2FA (SwissId app push) if required
//! 5.  Anomaly detection (device print), follow next URL
//! 6.  Extract SAML response + relay state
//! 7.  Exchange SAML for an OAuth code, then for access/refresh tokens
use base64::Engine;
use rand::Rng;
use sha2::Sha256;
use sha2::Digest;
use std::iter::Iterator as _;
use url::form_urlencoded;

use super::types::Token;
use super::{CLIENT_ID, CLIENT_SECRET, REDIRECT_URI, USER_AGENT};

const SWISSID_BASE: &str = "https://login.swissid.ch/api-login";
const PCC_BASE: &str = "https://pccweb.api.post.ch";

/// Token service contract (port of `ITokenService`).
#[async_trait::async_trait]
pub trait TokenService: Send + Sync {
    /// Login and token generation.
    async fn get_token(&self, username: &str, password: &str) -> anyhow::Result<Token>;
    /// Refresh token with a previously received refresh token.
    async fn refresh_token(&self, refresh_token: &str) -> anyhow::Result<Token>;
}

/// SwissId login service.
#[derive(Debug, Default, Clone)]
pub struct SwissIdLoginService {
    /// Cookie-sharing client used for the whole login flow.
    client: reqwest::Client,
    /// Fresh client for token endpoints (no shared cookies).
    token_client: reqwest::Client,
}

impl SwissIdLoginService {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .cookie_store(true)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(USER_AGENT)
            .build()
            .expect("cookie client");
        let token_client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(USER_AGENT)
            .build()
            .expect("token client");
        Self {
            client,
            token_client,
        }
    }

    fn url_query_string(&self, goto_parameter: &str) -> String {
        format!(
            "locale=en&goto={goto_parameter}&acr_values=loa-1&realm=%2Fsesam&service=qoa1"
        )
    }

    /// Follow a redirect chain until there are no further redirects,
    /// mirroring `SwissIdLoginHelper.FollowRedirect`. Returns every response.
    /// Follow a redirect chain until there are no further redirects,
    /// mirroring `SwissIdLoginHelper.FollowRedirect`. Returns the final response.
    async fn follow_redirect(&self, req: reqwest::RequestBuilder) -> anyhow::Result<reqwest::Response> {
        let mut res = req.send().await?;
        while res.status().is_redirection() {
            let location = res
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow::anyhow!("redirect response without Location"))?
                .to_string();
            let url = if location.starts_with("http") {
                location
            } else {
                let base = res.url().as_str();
                let origin = base.split_once("/").map(|(a, _)| a).unwrap_or(base);
                format!("{}{}", origin, location)
            };
            res = self.client.get(url).send().await?;
        }
        Ok(res)
    }

    /// 1. PCC web authorization - seed cookies.
    async fn pcc_web_authorization(&self, code_challenge: &str) -> anyhow::Result<()> {
        let params: Vec<(&str, String)> = vec![
            ("client_id", CLIENT_ID.into()),
            ("response_type", "code".into()),
            ("redirect_uri", REDIRECT_URI.into()),
            ("scope", "PCCWEB offline_access".into()),
            ("response_mode", "query".into()),
            ("state", "abcd".into()),
            ("code_challenge", code_challenge.into()),
            ("code_challenge_method", "S256".into()),
            ("lang", "en".into()),
        ];
        let query: Vec<String> = params
            .iter()
            .map(|(k, v)| {
                let v = form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>();
                format!("{}={}", k, v)
            })
            .collect();
        let url = format!("{PCC_BASE}/OAuth/authorization?{}", query.join("&"));

        let res = self.follow_redirect(self.client.get(url)).await?;
        if !res.status().is_success() {
            anyhow::bail!("PccWebAuthorization failed");
        }
        Ok(())
    }

    /// 2. Swiss Post login - extract the `goto` parameter.
    async fn swiss_post_login(&self) -> anyhow::Result<String> {
        let url = format!(
            "https://account.post.ch/idp/?login\
             &targetURL=https://pccweb.api.post.ch/SAML/ServiceProvider/\
             ?redirect_uri={REDIRECT_URI}\
             &profile=default\
             &app=pccwebapi\
             &inMobileApp=true\
             &layoutType=standard"
        );
        // account.post.ch 302-redirects through several hops; we need the last Location header.
        let first = self
            .client
            .post(&url)
            .form(&[("externalIDP", "externalIDP")])
            .send()
            .await?;
        let mut locations: Vec<String> = Vec::new();
        if let Some(v) = first.headers().get(reqwest::header::LOCATION) {
            if let Ok(s) = v.to_str() { locations.push(s.to_string()); }
        }
        let mut current = first;
        while current.status().is_redirection() {
            let loc = locations.last().cloned().ok_or_else(|| anyhow::anyhow!("no location"))?;
            let abs = if loc.starts_with("http") { loc } else { "https://account.post.ch".to_string() + &loc };
            let next = self.client.get(&abs).send().await?;
            if let Some(v) = next.headers().get(reqwest::header::LOCATION) {
                if let Ok(s) = v.to_str() { locations.push(s.to_string()); }
            }
            current = next;
        }
        let goto_query = locations
            .iter()
            .filter_map(|loc| loc.split_once('?').map(|(_, q)| q.to_string()))
            .find(|q| q.contains("goto="))
            .ok_or_else(|| anyhow::anyhow!("No goto parameter found"))?;

        let value = goto_query
            .split("goto=")
            .nth(1)
            .ok_or_else(|| anyhow::anyhow!("No goto parameter found"))?
            .split('&')
            .next()
            .unwrap_or_default()
            .to_string();
        Ok(value)
    }

    /// 3a. Additional session cookie.
    async fn swissid_api_login_token(&self, goto_parameter: &str) -> anyhow::Result<()> {
        let url = format!(
            "{SWISSID_BASE}/authenticate/token/status?locale=en&goto={goto_parameter}&acr_values=loa-1&realm=%2Fsesam&service=qoa1"
        );
        self.follow_redirect(self.client.get(url)).await?;
        Ok(())
    }

    /// 3b. Additional session cookie.
    async fn swissid_api_login_welcome_pack(&self, goto_parameter: &str) -> anyhow::Result<()> {
        let url = format!(
            "{SWISSID_BASE}/welcome-pack?locale=en&{goto_parameter}&acr_values=loa-1&realm=%2Fsesam&service=qoa1"
        );
        self.follow_redirect(self.client.get(url)).await?;
        Ok(())
    }

    /// 3c. Get authId.
    async fn swissid_login_authenticate_init(&self, goto_parameter: &str) -> anyhow::Result<String> {
        let url = format!(
            "{SWISSID_BASE}/authenticate/init?locale=en&goto={goto_parameter}&acr_values=loa-1&realm=%2Fsesam&service=qoa1"
        );
        let res = self.follow_redirect(self.client.post(url)).await?;
        let body: serde_json::Value = res.json().await?;
        body["tokens"]["authId"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("Missing authId"))
    }

    /// 3d. Basic auth - get next action type.
    async fn swissid_login_authenticate_basic(
        &self,
        url_query: &str,
        auth_id: &str,
        username: &str,
        password: &str,
    ) -> anyhow::Result<(String, String)> {
        let url = format!("{SWISSID_BASE}/authenticate/basic?{url_query}");
        let payload = serde_json::json!({ "username": username, "password": password });
        let builder = self
            .client
            .post(&url)
            .header("authId", auth_id)
            .json(&payload);
        let res = self.follow_redirect(builder).await?;
        let body: serde_json::Value = res.json().await?;

        let next_action = body["nextAction"]["type"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Next action type not found"))?
            .to_string();
        let auth_id = body["tokens"]["authId"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("AuthId not found"))?
            .to_string();
        Ok((next_action, auth_id))
    }

    /// 4. Wait until 2FA (SwissId app) is approved.
    async fn swissid_login_check_two_fa_status(
        &self,
        next_action_type: &str,
        url_query: &str,
        mut auth_id: String,
    ) -> anyhow::Result<String> {
        let started = std::time::Instant::now();
        let mut current = next_action_type.to_string();
        while current == "WAIT_FOR_ASYNC_SWISS_ID_APP_AUTHENTICATION"
            && started.elapsed() < std::time::Duration::from_secs(120)
        {
            let url = format!("{SWISSID_BASE}/authenticate/swiss-id-app/status?{url_query}");
            let res = self
                .follow_redirect(self.client.get(&url).header("authId", auth_id.clone()))
                .await?;
            let body: serde_json::Value = res.json().await?;

            auth_id = body["tokens"]["authId"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing authId"))?
                .to_string();
            current = body["nextAction"]["type"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Next action type not found"))?
                .to_string();

            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
        Ok(auth_id)
    }

    /// 5. Anomaly detection - get next URL for SAML.
    async fn swissid_anomaly_detection(
        &self,
        auth_id: &str,
        url_query: &str,
    ) -> anyhow::Result<String> {
        let app_version = USER_AGENT.trim_start_matches("Mozilla/");
        let payload = serde_json::json!({
            "appCodeName": "Mozilla",
            "appName": "Netscape",
            "appVersion": app_version,
            "fonts": {
                "installedFonts": "cursive;monospace;serif;sans-serif;fantasy;default;Arial;Courier;Courier New;Georgia;Tahoma;Times;Times New Roman;Verdana"
            },
            "language": "de",
            "platform": "Linux x86_64",
            "plugins": { "installedPlugins": "" },
            "product": "Gecko",
            "productSub": "20030107",
            "screen": {
                "screenColourDepth": 24,
                "screenHeight": 732,
                "screenWidth": 412
            },
            "timezone": { "timezone": -120 },
            "userAgent": USER_AGENT,
            "vendor": "Google Inc."
        });
        let url = format!("{SWISSID_BASE}/anomaly-detection/device-print?{url_query}");
        let res = self
            .client
            .post(&url)
            .header("authId", auth_id)
            .json(&payload)
            .send()
            .await?;
        let body: serde_json::Value = res.json().await?;
        body["nextAction"]["successUrl"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("SuccessUrl not found"))
    }

    /// 6a. Get the next URL to follow.
    async fn swissid_get_next_url(&self, url: &str) -> anyhow::Result<String> {
        let res = self.follow_redirect(self.client.get(url)).await?;
        let content = res.text().await?;
        let re = regex::Regex::new(r#"action="([^"]+)""#).unwrap();
        let next_url = re
            .captures(&content)
            .and_then(|c| c.get(1))
            .ok_or_else(|| anyhow::anyhow!("action= not found"))?
            .as_str()
            .to_string();
        Ok(next_url.replace(|c: char| c.is_whitespace(), ""))
    }

    /// 6b. Get SAML response + relay state.
    async fn swissid_get_token_and_relay_state(
        &self,
        next_url: &str,
    ) -> anyhow::Result<(String, String)> {
        let res = self.client.post(next_url).send().await?;
        let content = res.text().await?;

        let saml_re = regex::Regex::new(r#"name="SAMLResponse" value="([^"]+)""#).unwrap();
        let relay_re = regex::Regex::new(r#"name="RelayState" value="([^"]+)""#).unwrap();
        let saml = saml_re
            .captures(&content)
            .and_then(|c| c.get(1))
            .ok_or_else(|| anyhow::anyhow!("SAMLResponse not found"))?
            .as_str()
            .to_string();
        let relay = relay_re
            .captures(&content)
            .and_then(|c| c.get(1))
            .ok_or_else(|| anyhow::anyhow!("RelayState not found"))?
            .as_str()
            .to_string();
        Ok((saml, relay))
    }

    /// 7a. Exchange SAML for OAuth code.
    async fn pcc_web_oauth(&self, saml_token: &str, relay_state: &str) -> anyhow::Result<String> {
        let res = self
            .client
            .post(format!("{PCC_BASE}/OAuth/"))
            .header("Origin", "https://account.post.ch")
            .header("X-Requested-With", "ch.post.it.pcc")
            .header("Upgrade-Insecure-Requests", "1")
            .form(&[("RelayState", relay_state), ("SAMLResponse", saml_token)])
            .send()
            .await?;
        let location = res
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| anyhow::anyhow!("Query not found"))?;
        let query = location
            .split_once('?')
            .map(|(_, q)| q)
            .ok_or_else(|| anyhow::anyhow!("Query not found"))?;
        let code = form_urlencoded::parse(query.as_bytes())
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.to_string())
            .ok_or_else(|| anyhow::anyhow!("Code not found"))?;
        Ok(code)
    }

    /// 7b. Exchange code for tokens.
    async fn pcc_web_token(&self, code: &str, code_verifier: &str) -> anyhow::Result<serde_json::Value> {
        let res = self
            .token_client
            .post(format!("{PCC_BASE}/OAuth/token"))
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", CLIENT_ID),
                ("client_secret", CLIENT_SECRET),
                ("code", code),
                ("code_verifier", code_verifier),
                ("redirect_uri", REDIRECT_URI),
            ])
            .send()
            .await?;
        let body: serde_json::Value = res.json().await?;
        Ok(body)
    }

    /// Refresh token endpoint.
    async fn pcc_web_refresh_token(
        &self,
        refresh_token: &str,
    ) -> anyhow::Result<serde_json::Value> {
        let res = self
            .token_client
            .post(format!("{PCC_BASE}/OAuth/token"))
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", CLIENT_ID),
                ("client_secret", CLIENT_SECRET),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await?;
        let body: serde_json::Value = res.json().await?;
        Ok(body)
    }

    fn set_token(token_object: &serde_json::Value) -> anyhow::Result<Token> {
        let expires_in = token_object["expires_in"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("Missing expires in attribute"))?;
        Ok(Token {
            access_token: token_object["access_token"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing access token attribute"))?
                .to_string(),
            refresh_token: token_object["refresh_token"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing refresh token attribute"))?
                .to_string(),
            expires_in_seconds: expires_in,
            expires_at: chrono::Utc::now() + chrono::Duration::seconds(expires_in as i64),
        })
    }
}

#[async_trait::async_trait]
impl TokenService for SwissIdLoginService {
    async fn get_token(&self, username: &str, password: &str) -> anyhow::Result<Token> {
        let (code_verifier, code_challenge) = create_random_token();

        // 1. PCC web authorization - cookies
        self.pcc_web_authorization(&code_challenge).await?;

        // 2. Swiss post login - goto parameter extraction
        let goto_parameter = self.swiss_post_login().await?;

        let url_query = self.url_query_string(&goto_parameter);

        // 3a/3b. SwissId api login - additional cookies
        self.swissid_api_login_token(&goto_parameter).await?;
        self.swissid_api_login_welcome_pack(&goto_parameter).await?;

        // 3c. SwissId login init - authId
        let auth_id = self.swissid_login_authenticate_init(&goto_parameter).await?;

        // 3d. Basic auth - next action type
        let (next_action_type, auth_id) =
            self.swissid_login_authenticate_basic(&url_query, &auth_id, username, password)
                .await?;

        // 4. Wait for 2FA if needed
        let auth_id = if next_action_type == "WAIT_FOR_ASYNC_SWISS_ID_APP_AUTHENTICATION" {
            self.swissid_login_check_two_fa_status(&next_action_type, &url_query, auth_id)
                .await?
        } else {
            tracing::warn!("unexpected next action type: {next_action_type}");
            auth_id
        };

        // 5. Anomaly detection - next url for SAML
        let next_url = self.swissid_anomaly_detection(&auth_id, &url_query).await?;

        // 6a. Get next url from next url
        let next_url = self.swissid_get_next_url(&next_url).await?;

        // 6b. SAML response + relay state
        let (saml_token, relay_state) = self.swissid_get_token_and_relay_state(&next_url).await?;

        // 7a. PCC web OAuth - code
        let code = self.pcc_web_oauth(&saml_token, &relay_state).await?;

        // 7b. PCC web token - access + refresh token
        let token_object = self.pcc_web_token(&code, &code_verifier).await?;
        Self::set_token(&token_object)
    }

    async fn refresh_token(&self, refresh_token: &str) -> anyhow::Result<Token> {
        let token_object = self.pcc_web_refresh_token(refresh_token).await?;
        Self::set_token(&token_object)
    }
}

/// Create random token (code verifier + S256 code challenge) with 64 bytes.
pub fn create_random_token() -> (String, String) {
    let mut rng = rand::rng();
    let random_bytes: [u8; 64] = rng.random();
    let random_string = url_safe_base64_encode(&random_bytes);
    let hash = <Sha256 as Digest>::digest(random_string.as_bytes());
    (random_string, url_safe_base64_encode(&hash))
}

/// URL-safe base64 without padding.
fn url_safe_base64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_token_shapes() {
        let (verifier, challenge) = create_random_token();
        // 64 bytes -> 86 base64 chars (no padding)
        assert_eq!(verifier.len(), 86);
        // 32 sha256 bytes -> 43 base64 chars (no padding)
        assert_eq!(challenge.len(), 43);
        assert!(verifier.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }
}
