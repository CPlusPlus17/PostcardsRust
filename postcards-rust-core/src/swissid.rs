//! SwissId login flow (port of `PostcardsDotnet.Services.SwissIdLoginService`).
//!
//! HTTP layer: the `curl` crate, linked against the **system libcurl 8.18**
//! (via the `PKG_CONFIG` shim in `~/.libcurl-pc`), because that ClientHello is
//! the one the swissid WAF accepts. `reqwest` (native-tls/rustls) is reset
//! mid-handshake (error 35) by the same WAF, so it cannot be used for the
//! swissid endpoints. See `POSTCARDSRUST_HTTP.md` / build notes.
//!
//! The curl crate is synchronous; every network step runs on a blocking thread
//! (`tokio::task::spawn_blocking`). A single in-memory cookie jar is shared
//! across the whole flow, mirroring the .NET `CookieContainer`.
//!
//! Steps (identical to the .NET implementation):
//! 1.  PCC web OAuth authorization (seed cookies)
//! 2.  Swiss Post IdP login, follow SAML redirects, extract `goto`
//! 3.  SwissId api-login: token/status, welcome-pack, init (authId), basic
//! 4.  Wait for 2FA (SwissId app push) if required
//! 5.  Anomaly detection (device print), follow next URL
//! 6.  Extract SAML response + relay state
//! 7.  Exchange SAML for an OAuth code, then for access/refresh tokens
use base64::Engine;
use curl::easy::{Easy, List};
use rand::Rng;
use sha2::Digest;
use sha2::Sha256;
use std::collections::HashMap;
use std::time::Duration;
use url::form_urlencoded;

use super::types::Token;
use super::{CLIENT_ID, CLIENT_SECRET, REDIRECT_URI, USER_AGENT};

const SWISSID_BASE: &str = "https://login.swissid.ch/api-login";
const PCC_BASE: &str = "https://pccweb.api.post.ch";

// ===========================================================================
// Shared cookie jar (in-memory). Mirrors the .NET CookieContainer well enough
// for this flow: store by (domain, name), match by exact host or parent domain.
// ===========================================================================

#[derive(Default, Clone)]
struct CookieJar {
    /// (domain, name) -> value.
    map: HashMap<(String, String), String>,
}

impl CookieJar {
    fn set(&mut self, domain: &str, name: &str, value: &str) {
        self.map.insert((domain.to_string(), name.to_string()), value.to_string());
    }
    fn header_for(&self, url: &str) -> Option<String> {
        let host = host_of(url);
        let mut pairs: Vec<String> = Vec::new();
        for ((domain, name), value) in &self.map {
            if domain.as_str() == host || host.ends_with(&format!(".{domain}")) {
                pairs.push(format!("{name}={value}"));
            }
        }
        if pairs.is_empty() {
            None
        } else {
            Some(pairs.join("; "))
        }
    }
    fn absorb_set_cookie(&mut self, url: &str, sc: &str) {
        let mut name = String::new();
        let mut value = String::new();
        let mut domain: Option<String> = None;
        for part in sc.split(';') {
            let p = part.trim();
            if name.is_empty() {
                if let Some(idx) = p.find('=') {
                    name = p[..idx].trim().to_string();
                    value = p[idx + 1..].trim().to_string();
                }
            } else if let Some(d) = p.to_ascii_lowercase().strip_prefix("domain=") {
                domain = Some(d.trim().trim_start_matches('.').to_string());
            }
        }
        if name.is_empty() {
            return;
        }
        let d = domain.unwrap_or_else(|| host_of(url));
        self.set(&d, &name, &value);
    }
}

/// One HTTP response (no auto-redirect).
struct Resp {
    status: u32,
    location: Option<String>,
    body: String,
    set_cookies: Vec<String>,
    error: Option<String>,
}

// ===========================================================================
// Request helpers (synchronous — run inside spawn_blocking).
// ===========================================================================

/// Perform one request. Reads cookies from the jar; returns the response and
/// the raw `Set-Cookie` headers (caller applies them).
fn one(jar: &CookieJar, url: &str, method: &str, body: Option<&str>, extra: &[(&str, &str)]) -> Resp {
    let mut e = Easy::new();
    let r = (|| -> Result<Resp, String> {
        e.url(url).map_err(|e| e.to_string())?;
        e.timeout(Duration::from_secs(45)).map_err(|e| e.to_string())?;
        e.useragent(USER_AGENT).map_err(|e| e.to_string())?;
        // Force HTTP/1.1 to match the .NET HttpClient (and the working CLI).
        e.http_version(curl::easy::HttpVersion::V11).map_err(|e| e.to_string())?;

        match method {
            "GET" => { e.get(true).map_err(|e| e.to_string())?; }
            "POST" => {
                e.post(true).map_err(|e| e.to_string())?;
                if body.is_none() { e.post_field_size(0).map_err(|e| e.to_string())?; }
            }
            _ => {}
        }

        if let Some(b) = body {
            e.post_field_size(b.len() as u64).map_err(|e| e.to_string())?;
            let buf = std::sync::Arc::new(std::sync::Mutex::new(b.as_bytes().to_vec()));
            let buf2 = buf.clone();
            e.read_function(move |out| {
                let mut m = buf2.lock().unwrap();
                let n = std::cmp::min(out.len(), m.len());
                out[..n].copy_from_slice(&m[..n]);
                m.drain(..n);
                Ok(n)
            }).map_err(|e| e.to_string())?;
        }

        // Request headers (explicit ones + shared Cookie).
        let mut headers: Vec<String> = Vec::new();
        for (k, v) in extra {
            headers.push(format!("{k}: {v}"));
        }
        if let Some(c) = jar.header_for(url) {
            headers.push(format!("Cookie: {c}"));
        }
        if !headers.is_empty() {
            let mut lst = List::new();
            for h in &headers {
                lst.append(h).map_err(|e| e.to_string())?;
            }
            e.http_headers(lst).map_err(|e| e.to_string())?;
        }

        let body_out = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let bo2 = body_out.clone();
        e.write_function(move |d| {
            bo2.lock().unwrap().extend_from_slice(d);
            Ok(d.len())
        }).map_err(|e| e.to_string())?;

        let location = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let lb2 = location.clone();
        let setc = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sc2 = setc.clone();
        e.header_function(move |h: &[u8]| {
            let s = String::from_utf8_lossy(h);
            let up = s.trim_start().to_ascii_uppercase();
            if up.starts_with("LOCATION:") {
                let val = s.splitn(2, ':').nth(1).unwrap_or("").trim().to_string();
                *lb2.lock().unwrap() = val;
            } else if up.starts_with("SET-COOKIE:") {
                let val = s.splitn(2, ':').nth(1).unwrap_or("").trim().to_string();
                sc2.lock().unwrap().push(val);
            }
            true
        }).map_err(|e| e.to_string())?;

        e.perform().map_err(|e| e.to_string())?;
        let status = e.response_code().map_err(|e| e.to_string())?;
        let body = String::from_utf8_lossy(&body_out.lock().unwrap()).to_string();
        let location = location.lock().unwrap().clone();
        let set_cookies = setc.lock().unwrap().clone();
        Ok(Resp {
            status,
            location: if location.is_empty() { None } else { Some(location) },
            body,
            set_cookies,
            error: None,
        })
    })();
    r.unwrap_or_else(|e| Resp {
        status: 0,
        location: None,
        body: String::new(),
        set_cookies: Vec::new(),
        error: Some(e),
    })
}

/// Follow a redirect chain (preserving method/body, like the .NET FollowRedirect),
/// absorbing Set-Cookie into the jar after each hop. Returns the final response
/// plus the ordered Location headers seen.
fn follow(
    jar: &mut CookieJar,
    url: &str,
    method: &str,
    body: Option<&str>,
    extra: &[(&str, &str)],
    max_hops: usize,
) -> (Resp, Vec<String>) {
    let mut cur = url.to_string();
    let mut locs = Vec::new();
    let mut last = Resp {
        status: 0, location: None, body: String::new(), set_cookies: Vec::new(), error: Some("maxhops".into()),
    };
    for _ in 0..=max_hops {
        let j = jar.clone();
        let r = one(&j, &cur, method, body, extra);
        for sc in &r.set_cookies {
            jar.absorb_set_cookie(&cur, sc);
        }
        if (300..400).contains(&r.status) && r.location.is_some() {
            let loc = r.location.clone().unwrap();
            locs.push(loc.clone());
            cur = if loc.starts_with("http") {
                loc
            } else {
                let host = host_of(&cur);
                if loc.starts_with('/') {
                    format!("https://{host}{loc}")
                } else {
                    format!("https://{host}/{loc}")
                }
            };
            last = r;
            continue;
        }
        last = r;
        break;
    }
    (last, locs)
}

/// Host without port (cookies are port-agnostic per RFC 6265).
fn host_of(url: &str) -> String {
    let h = url
        .split_once("//")
        .and_then(|(_, rest)| rest.split('/').next())
        .unwrap_or_default()
        .to_string();
    h.split(':').next().unwrap_or_default().to_string()
}

// ===========================================================================
// SwissId login service (public, async facade).
// ===========================================================================

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
pub struct SwissIdLoginService;

impl SwissIdLoginService {
    pub fn new() -> Self {
        Self
    }

    /// Run the whole login flow on a blocking thread.
    fn run_sync(
        username: &str,
        password: &str,
    ) -> anyhow::Result<Token> {
        let (code_verifier, code_challenge) = create_random_token();
        let mut jar = CookieJar::default();

        // 1. PCC web authorization - seed cookies.
        let url = format!(
            "{PCC_BASE}/OAuth/authorization?client_id={}&response_type=code&redirect_uri={}&scope=PCCWEB%20offline_access&response_mode=query&state=abcd&code_challenge={}&code_challenge_method=S256&lang=en",
            form_urlencoded::byte_serialize(CLIENT_ID.as_bytes()).collect::<String>(),
            form_urlencoded::byte_serialize(REDIRECT_URI.as_bytes()).collect::<String>(),
            form_urlencoded::byte_serialize(code_challenge.as_bytes()).collect::<String>(),
        );
        let (r, l1) = follow(&mut jar, &url, "GET", None, &[], 6);
        tracing::debug!(status = r.status, hops = l1.len(), "PCC web authorization");

        // 2. Swiss Post login - extract the `goto` parameter.
        let post_url = l1
            .first()
            .map(|loc| {
                if loc.contains("idp/?") {
                    loc.replace("idp/?", "idp/?login&")
                } else {
                    loc.clone()
                }
            })
            .unwrap_or_else(|| {
                format!(
                    "https://account.post.ch/idp/?login&targetURL={}&redirect_uri={}&lang=en&profile=default&app=pccwebapi&inMobileApp=true&layoutType=standard",
                    form_urlencoded::byte_serialize(format!("{PCC_BASE}/SAML/ServiceProvider/").as_bytes()).collect::<String>(),
                    form_urlencoded::byte_serialize(REDIRECT_URI.as_bytes()).collect::<String>(),
                )
            });
        let (r2, l2) = follow(
            &mut jar,
            &post_url,
            "POST",
            Some("externalIDP=externalIDP"),
            &[("Content-Type", "application/x-www-form-urlencoded")],
            12,
        );
        tracing::debug!(status = r2.status, hops = l2.len(), "Swiss Post login");
        let goto_query: Option<String> = l2
            .iter()
            .filter_map(|l| l.split_once('?').map(|(_, q)| q.to_string()))
            .rev()
            .find(|qq| qq.contains("goto="));
        let goto_parameter = goto_query
            .and_then(|q| q.split("goto=").nth(1).map(|s| s.to_string()))
            .and_then(|s| s.split('&').next().map(|s| s.to_string()))
            .ok_or_else(|| anyhow::anyhow!("No goto parameter found"))?;
        tracing::debug!(goto = %goto_parameter.chars().take(60).collect::<String>(), "goto extracted");

        let url_query = url_query_string(&goto_parameter);

        // 3a. token/status - extra cookie.
        let t = format!("{SWISSID_BASE}/authenticate/token/status?{url_query}");
        let (rt, _) = follow(&mut jar, &t, "GET", None, &[], 3);
        tracing::debug!(status = rt.status, "token/status");

        // 3b. welcome-pack - extra cookie (non-fatal: the WAF often 403s this).
        let w = format!("{SWISSID_BASE}/welcome-pack?locale=en&{goto_parameter}&acr_values=loa-1&realm=%2Fsesam&service=qoa1");
        let (rw, _) = follow(&mut jar, &w, "GET", None, &[], 3);
        if rw.status < 200 || rw.status >= 300 {
            tracing::debug!(status = rw.status, "welcome-pack (non-fatal)");
        } else {
            tracing::debug!(status = rw.status, "welcome-pack");
        }

        // 3c/3d. init (authId) + basic auth (nextAction) with retry
        let mut next_action_type = String::new();
        let mut auth_id = String::new();
        let mut last_basic_err = String::new();
        for attempt in 1..=6u32 {
            if attempt > 1 {
                std::thread::sleep(Duration::from_millis(500));
            }
            // 3c. init - authId.
            let i = format!("{SWISSID_BASE}/authenticate/init?{url_query}");
            let (ri, _) = follow(&mut jar, &i, "POST", None, &[], 3);
            let aid = match extract_auth_id(&ri.body) {
                Ok(a) => a,
                Err(e) => {
                    last_basic_err = format!("init failed: {e}");
                    tracing::warn!(attempt, "init failed: {e}");
                    continue;
                }
            };
            tracing::debug!(status = ri.status, attempt, "init -> authId");

            // 3d. basic auth - next action type.
            let basic_url = format!("{SWISSID_BASE}/authenticate/basic?{url_query}");
            let payload = serde_json::json!({ "username": username, "password": password }).to_string();
            let (rb, _) = follow(
                &mut jar,
                &basic_url,
                "POST",
                Some(&payload),
                &[("authId", aid.as_str()), ("Content-Type", "application/json")],
                3,
            );
            if let Ok(basic) = serde_json::from_str::<serde_json::Value>(&rb.body) {
                if let (Some(nat), Some(naid)) = (
                    basic["nextAction"]["type"].as_str(),
                    basic["tokens"]["authId"].as_str(),
                ) {
                    next_action_type = nat.to_string();
                    auth_id = naid.to_string();
                    tracing::debug!(next_action_type = %next_action_type, "basic");
                    tracing::debug!(basic_body = %rb.body, "basic full response (for MTAN contract)");
                    break;
                }
            }
            if rb.body.contains("NEW_OTP_NOT_ALLOWED_CODE") {
                tracing::info!(attempt, "SwissID OTP cooldown in effect, waiting 15s before retrying basic auth...");
                std::thread::sleep(Duration::from_secs(15));
                continue;
            }
            last_basic_err = format!("basic failed: status={} body={}", rb.status, rb.body);
            tracing::warn!(attempt, "basic auth attempt failed: {last_basic_err}");
        }
        if next_action_type.is_empty() {
            anyhow::bail!("Next action type not found after attempts: {last_basic_err}");
        }

        // 4. Two-factor.
        //   - WAIT_FOR_ASYNC_SWISS_ID_APP_AUTHENTICATION: poll the app status.
        //   - AUTHENTICATE_MTAN: submit the mobile text code (POST /authenticate/mtan {code}).
        let mut auth_id = auth_id;
        if next_action_type == "WAIT_FOR_ASYNC_SWISS_ID_APP_AUTHENTICATION" {
            let started = std::time::Instant::now();
            let mut current = next_action_type.clone();
            while current == "WAIT_FOR_ASYNC_SWISS_ID_APP_AUTHENTICATION"
                && started.elapsed() < Duration::from_secs(120)
            {
                let s = format!("{SWISSID_BASE}/authenticate/swiss-id-app/status?{url_query}");
                let (rs, _) = follow(&mut jar, &s, "GET", None, &[("authId", auth_id.as_str())], 3);
                let v: serde_json::Value = serde_json::from_str(&rs.body)?;
                auth_id = v["tokens"]["authId"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing authId"))?
                    .to_string();
                current = v["nextAction"]["type"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Next action type not found"))?
                    .to_string();
                std::thread::sleep(Duration::from_secs(3));
            }
            tracing::debug!(next_action_type = %current, "2FA wait done");
        } else if next_action_type == "AUTHENTICATE_MTAN" {
            loop {
                let code = read_mtan_code()?;
                tracing::debug!("submitting MTAN code");
                let mtan_url = format!("{SWISSID_BASE}/authenticate/mtan?{url_query}");
                let body = serde_json::json!({ "code": code }).to_string();
                let (rm, _) = follow(
                    &mut jar,
                    &mtan_url,
                    "POST",
                    Some(&body),
                    &[("authId", auth_id.as_str()), ("Content-Type", "application/json")],
                    3,
                );
                tracing::debug!(status = rm.status, "MTAN submit");
                std::fs::write("/tmp/e2e_mtan.json", rm.body.as_str()).ok();
                tracing::debug!(mtan_body = %rm.body.chars().take(400).collect::<String>(), "MTAN response");
                let v: serde_json::Value = serde_json::from_str(&rm.body)
                    .map_err(|e| anyhow::anyhow!("failed to parse MTAN response JSON: {e}, body: {}", rm.body))?;

                if let Some(a) = v["tokens"]["authId"].as_str().or_else(|| v["authId"].as_str()) {
                    auth_id = a.to_string();
                }

                if rm.status == 200 {
                    let after = v["nextAction"]["type"].as_str().unwrap_or("(none)").to_string();
                    tracing::debug!(next_action_type = %after, "after MTAN");
                    break;
                } else if v["errorCode"].as_str() == Some("INVALID_MTAN_CODE") {
                    tracing::warn!("Invalid 2FA MTAN code. Waiting for fresh code in /tmp/pcd_2fa_code...");
                    continue;
                } else {
                    anyhow::bail!("MTAN submission failed: status={} body={}", rm.status, rm.body);
                }
            }
        } else {
            tracing::warn!("unexpected next action type: {next_action_type}");
        }

        // 5. Anomaly detection - get next URL for SAML.
        let anomaly = anomaly_detection_payload();
        let ad_url = format!("{SWISSID_BASE}/anomaly-detection/device-print?{url_query}");
        let (ra, _) = follow(
            &mut jar,
            &ad_url,
            "POST",
            Some(&anomaly),
            &[("authId", auth_id.as_str()), ("Content-Type", "application/json")],
            3,
        );
        if ra.status < 200 || ra.status >= 300 {
            anyhow::bail!("anomaly detection failed: status={} body={}", ra.status, ra.body);
        }
        let v: serde_json::Value = serde_json::from_str(&ra.body)
            .map_err(|e| anyhow::anyhow!("failed to parse anomaly detection response: {e}, body: {}", ra.body))?;
        let next_url = v["nextAction"]["successUrl"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("SuccessUrl not found in anomaly response: {}", ra.body))?
            .to_string();
        tracing::debug!(next_url = %next_url, "anomaly -> successUrl");
        std::fs::write("/tmp/e2e_anomaly.json", ra.body.as_str()).ok();

        // [DEBUG] dump cookie jar + what header_for would send for successUrl
        let dbg = jar
            .map
            .iter()
            .map(|((d, n), v)| format!("{d}\t{n}={v}"))
            .collect::<Vec<_>>()
            .join("\n");
        tracing::debug!(jar_entries = %dbg, "cookie jar before successUrl");
        tracing::debug!(header_for_successurl = ?jar.header_for(&next_url), "cookie header that WILL be sent");
        std::fs::write("/tmp/e2e_cookiejar.txt", dbg.as_str()).ok();

        // 6a. Get the next URL to follow.
        let (rn, ln) = follow(&mut jar, &next_url, "GET", None, &[], 6);
        tracing::debug!(
            status = rn.status,
            hops = ln.len(),
            body_len = rn.body.len(),
            "next-url fetch"
        );
        for (i, l) in ln.iter().enumerate() {
            tracing::debug!(hop = i, location = %l, "redirect hop");
        }
        tracing::debug!(body_preview = %rn.body.chars().take(400).collect::<String>(), "next-url body");
        std::fs::write("/tmp/e2e_nexturl.html", rn.body.as_str()).ok();
        let next_url = regex::Regex::new(r#"action="([^"]+)"#)
            .unwrap()
            .captures(&rn.body)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .ok_or_else(|| anyhow::anyhow!("action= not found"))?
            .replace(|c: char| c.is_whitespace(), "");

        // 6b. SAML response + relay state.
        let (rf, _) = follow(&mut jar, &next_url, "POST", None, &[], 3);
        tracing::debug!(
            status = rf.status,
            body_len = rf.body.len(),
            has_saml = rf.body.contains("SAMLResponse"),
            has_relay = rf.body.contains("RelayState"),
            "SAML POST (step 6b)"
        );
        std::fs::write("/tmp/e2e_samlpost.html", rf.body.as_str()).ok();
        let saml_re = regex::Regex::new(r#"name="SAMLResponse" value="([^"]+)""#).unwrap();
        let relay_re = regex::Regex::new(r#"name="RelayState" value="([^"]+)""#).unwrap();
        let saml_token = saml_re
            .captures(&rf.body)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .ok_or_else(|| anyhow::anyhow!("SAMLResponse not found"))?;
        let relay_state = relay_re
            .captures(&rf.body)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .ok_or_else(|| anyhow::anyhow!("RelayState not found"))?;
        let acs_url = regex::Regex::new(r#"action="([^"]+)""#)
            .ok()
            .and_then(|re| re.captures(&rf.body))
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .unwrap_or_else(|| format!("{PCC_BASE}/OAuth/"));
        tracing::debug!(acs_url = %acs_url, "SAML response + relay state captured");

        // 7a. Exchange SAML for OAuth code.
        let oauth_body = format!(
            "RelayState={}&SAMLResponse={}",
            form_urlencoded::byte_serialize(relay_state.as_bytes()).collect::<String>(),
            form_urlencoded::byte_serialize(saml_token.as_bytes()).collect::<String>(),
        );
        // One request, do NOT follow the 302: the OAuth code lives in the
        // `Location` header of the first redirect (-> ch.post.pcc://?code=...),
        // which curl can't follow (custom scheme). Mirrors the .NET no-redirect
        // HttpClient used here. The ACS is intermittently flaky (sometimes
        // returns the login page 200 instead of the 302), so retry a few times.
        let mut code: Option<String> = None;
        let mut last_status = 0u32;
        for attempt in 1..=4u32 {
            if attempt > 1 {
                std::thread::sleep(Duration::from_millis(700));
            }
            let ro = one(
                &mut jar,
                acs_url.as_str(),
                "POST",
                Some(&oauth_body),
                &[(
                    "Origin",
                    "https://account.post.ch",
                ), (
                    "X-Requested-With",
                    "ch.post.it.pcc",
                ), (
                    "Upgrade-Insecure-Requests",
                    "1",
                ), (
                    "Content-Type",
                    "application/x-www-form-urlencoded",
                )],
            );
            tracing::debug!(
                attempt,
                status = ro.status,
                location = ?ro.location,
                body_len = ro.body.len(),
                "OAuth ACS (7a)"
            );
            std::fs::write(
                "/tmp/e2e_oauth_acs.txt",
                format!(
                    "status={}\nlocation={:?}\nerror={:?}\n{}",
                    ro.status, ro.location, ro.error, ro.body
                ),
            )
            .ok();
            for sc in &ro.set_cookies {
                jar.absorb_set_cookie(format!("{PCC_BASE}/OAuth/").as_str(), sc);
            }
            last_status = ro.status;
            if let Some(loc) = ro.location.as_ref() {
                if let Some((_, c)) = loc
                    .split_once('?')
                    .and_then(|(_, q)| {
                        form_urlencoded::parse(q.as_bytes()).find(|(k, _)| k == "code")
                    })
                {
                    code = Some(c.to_string());
                    break;
                }
            }
        }
        let code = code
            .ok_or_else(|| anyhow::anyhow!("OAuth code redirect missing (status={last_status})"))?;

        // 7b. Exchange code for tokens (requires a fresh session with no cookies, matching .NET PccWebToken).
        let token_body = format!(
            "grant_type=authorization_code&client_id={}&client_secret={}&code={}&code_verifier={}&redirect_uri={}",
            form_urlencoded::byte_serialize(CLIENT_ID.as_bytes()).collect::<String>(),
            form_urlencoded::byte_serialize(CLIENT_SECRET.as_bytes()).collect::<String>(),
            form_urlencoded::byte_serialize(code.as_bytes()).collect::<String>(),
            form_urlencoded::byte_serialize(code_verifier.as_bytes()).collect::<String>(),
            form_urlencoded::byte_serialize(REDIRECT_URI.as_bytes()).collect::<String>(),
        );
        let mut clean_jar = CookieJar::default();
        let (rtok, _) = follow(
            &mut clean_jar,
            format!("{PCC_BASE}/OAuth/token").as_str(),
            "POST",
            Some(&token_body),
            &[("Content-Type", "application/x-www-form-urlencoded")],
            2,
        );
        tracing::debug!(status = rtok.status, body = %rtok.body, "OAuth token exchange (7b)");
        if rtok.status < 200 || rtok.status >= 300 {
            anyhow::bail!("token exchange failed: status={} body={}", rtok.status, rtok.body);
        }
        let token_object: serde_json::Value = serde_json::from_str(&rtok.body)
            .map_err(|e| anyhow::anyhow!("failed to parse token JSON: {e}, body: {}", rtok.body))?;
        set_token(&token_object)
    }

    /// Refresh token endpoint.
    fn refresh_sync(refresh_token: &str) -> anyhow::Result<Token> {
        let mut jar = CookieJar::default();
        let body = format!(
            "grant_type=refresh_token&client_id={}&client_secret={}&refresh_token={}",
            form_urlencoded::byte_serialize(CLIENT_ID.as_bytes()).collect::<String>(),
            form_urlencoded::byte_serialize(CLIENT_SECRET.as_bytes()).collect::<String>(),
            form_urlencoded::byte_serialize(refresh_token.as_bytes()).collect::<String>(),
        );
        let (r, _) = follow(
            &mut jar,
            format!("{PCC_BASE}/OAuth/token").as_str(),
            "POST",
            Some(&body),
            &[("Content-Type", "application/x-www-form-urlencoded")],
            2,
        );
        if r.status != 200 {
            anyhow::bail!("token refresh failed: status={} body={}", r.status, r.body);
        }
        let token_object: serde_json::Value = serde_json::from_str(&r.body)
            .map_err(|e| anyhow::anyhow!("failed to parse token JSON: {e}, body: {}", r.body))?;
        set_token(&token_object)
    }
}

#[async_trait::async_trait]
impl TokenService for SwissIdLoginService {
    async fn get_token(&self, username: &str, password: &str) -> anyhow::Result<Token> {
        let (u, p) = (username.to_string(), password.to_string());
        tokio::task::spawn_blocking(move || SwissIdLoginService::run_sync(&u, &p))
            .await
            .map_err(|e| anyhow::anyhow!("login task join failed: {e}"))?
    }

    async fn refresh_token(&self, refresh_token: &str) -> anyhow::Result<Token> {
        let rt = refresh_token.to_string();
        tokio::task::spawn_blocking(move || SwissIdLoginService::refresh_sync(&rt))
            .await
            .map_err(|e| anyhow::anyhow!("refresh task join failed: {e}"))?
    }
}

// ===========================================================================
// Helpers
// ===========================================================================

fn url_query_string(goto_parameter: &str) -> String {
    format!(
        "locale=en&goto={goto_parameter}&acr_values=loa-1&realm=%2Fsesam&service=qoa1"
    )
}

/// Read the 2FA mobile-text code. Source order:
/// 1. `PCD_2FA_CODE` env var (if already set),
/// 2. the file at `PCD_2FA_CODE_FILE` (default `/tmp/pcd_2fa_code`) — polled
///    for up to 3 minutes so you can paste the code live while it's running.
fn read_mtan_code() -> anyhow::Result<String> {
    if let Ok(c) = std::env::var("PCD_2FA_CODE") {
        let c = c.trim();
        if !c.is_empty() {
            return Ok(c.to_string());
        }
    }
    let path = std::env::var("PCD_2FA_CODE_FILE").unwrap_or_else(|_| "/tmp/pcd_2fa_code".to_string());
    // Generous window (env-overridable) so a human can read the "waiting" log and
    // paste the SMS code without a timeout race. Default 6 minutes.
    let wait_secs: u64 = std::env::var("PCD_2FA_WAIT_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(360);
    let deadline = std::time::Instant::now() + Duration::from_secs(wait_secs);
    tracing::info!("waiting for 2FA mobile-text code (up to {wait_secs}s) — write it to {path} or set PCD_2FA_CODE");
    while std::time::Instant::now() < deadline {
        if let Ok(c) = std::fs::read_to_string(&path) {
            let c = c.trim().to_string();
            if !c.is_empty() {
                let _ = std::fs::remove_file(&path);
                tracing::info!("read 2FA code from {path}");
                return Ok(c);
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    anyhow::bail!("no 2FA code available (set PCD_2FA_CODE or write to {path})")
}

fn extract_auth_id(body: &str) -> anyhow::Result<String> {
    let v: serde_json::Value = serde_json::from_str(body)?;
    v["tokens"]["authId"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("Missing authId"))
}

fn anomaly_detection_payload() -> String {
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
    payload.to_string()
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

    #[test]
    fn cookie_jar_scopes_by_domain() {
        let mut j = CookieJar::default();
        j.absorb_set_cookie("https://pccweb.api.post.ch/x", "NavajoPCC=abc; Path=/; Secure");
        j.absorb_set_cookie("https://login.swissid.ch/x", "swissid=xyz; Path=/; Domain=swissid.ch");
        assert!(j.header_for("https://pccweb.api.post.com/nope").is_none());
        let h = j.header_for("https://pccweb.api.post.ch/o/auth").unwrap();
        assert!(h.contains("NavajoPCC=abc"));
        let h2 = j.header_for("https://login.swissid.ch/api-login").unwrap();
        assert!(h2.contains("swissid=xyz"));
    }
}
