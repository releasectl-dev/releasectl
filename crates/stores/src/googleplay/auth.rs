//! サービスアカウントの JWT-bearer OAuth2 フロー。アクセストークンは期限の
//! 少し前までキャッシュする（Go 版 `internal/googleplay/auth.go`）。

use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SCOPE: &str = "https://www.googleapis.com/auth/androidpublisher";

#[derive(serde::Deserialize)]
struct ServiceAccount {
    #[serde(default)]
    client_email: String,
    #[serde(default)]
    private_key: String,
    #[serde(default)]
    token_uri: String,
}

pub struct TokenSource {
    email: String,
    token_uri: String,
    key: jsonwebtoken::EncodingKey,
    http: reqwest::blocking::Client,
    cache: Mutex<Option<(String, SystemTime)>>,
}

#[derive(serde::Serialize)]
struct Claims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    iat: u64,
    exp: u64,
}

impl TokenSource {
    pub fn new(sa_json: &[u8], http: reqwest::blocking::Client) -> Result<Self, String> {
        let sa: ServiceAccount = serde_json::from_slice(sa_json)
            .map_err(|e| format!("parse service account json: {e}"))?;
        if sa.client_email.is_empty() || sa.private_key.is_empty() {
            return Err("service account json missing client_email or private_key".into());
        }
        let token_uri = if sa.token_uri.is_empty() {
            "https://oauth2.googleapis.com/token".to_string()
        } else {
            sa.token_uri
        };
        // PKCS#8（"PRIVATE KEY"）と PKCS#1（"RSA PRIVATE KEY"）の両方を受ける。
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(sa.private_key.as_bytes())
            .map_err(|e| format!("parse service account private key: {e}"))?;
        Ok(TokenSource {
            email: sa.client_email,
            token_uri,
            key,
            http,
            cache: Mutex::new(None),
        })
    }

    pub fn token(&self) -> Result<String, String> {
        let mut cache = self.cache.lock().unwrap();
        let now = SystemTime::now();
        if let Some((tok, expiry)) = cache.as_ref()
            && now + Duration::from_secs(60) < *expiry
        {
            return Ok(tok.clone());
        }
        let iat = now.duration_since(UNIX_EPOCH).unwrap().as_secs();
        let claims = Claims {
            iss: &self.email,
            scope: SCOPE,
            aud: &self.token_uri,
            iat,
            exp: iat + 3600,
        };
        let assertion = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
            &claims,
            &self.key,
        )
        .map_err(|e| e.to_string())?;

        let resp = self
            .http
            .post(&self.token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let body = resp.bytes().unwrap_or_default();

        #[derive(serde::Deserialize, Default)]
        struct Tr {
            #[serde(default)]
            access_token: String,
            #[serde(default)]
            expires_in: u64,
            #[serde(default)]
            error: String,
            #[serde(default)]
            error_description: String,
        }
        let tr: Tr = serde_json::from_slice(&body).unwrap_or_default();
        if tr.access_token.is_empty() {
            return Err(format!(
                "token exchange failed (HTTP {status}): {} {}",
                tr.error, tr.error_description
            ));
        }
        *cache = Some((
            tr.access_token.clone(),
            now + Duration::from_secs(tr.expires_in),
        ));
        Ok(tr.access_token)
    }
}
