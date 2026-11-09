//! App Store Connect API の ES256 JWT（Go 版 `internal/appstore/jwt.go`）。
//! Apple の要件: alg=ES256、aud=appstoreconnect-v1、exp は iat から 20 分以内。
//! 16 分で作り、期限の 60 秒前に作り直す。

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct TokenSource {
    key_id: String,
    issuer_id: String,
    key: jsonwebtoken::EncodingKey,
    cache: Mutex<Option<(String, u64)>>,
}

#[derive(serde::Serialize)]
struct Claims<'a> {
    iss: &'a str,
    iat: u64,
    exp: u64,
    aud: &'a str,
}

impl TokenSource {
    /// .p8（PKCS#8 の EC 鍵）を読む。
    pub fn new(key_id: String, issuer_id: String, pem: &[u8]) -> Result<Self, String> {
        let key = jsonwebtoken::EncodingKey::from_ec_pem(pem)
            .map_err(|e| format!("appstore: parse private key: {e}"))?;
        Ok(TokenSource {
            key_id,
            issuer_id,
            key,
            cache: Mutex::new(None),
        })
    }

    pub fn token(&self) -> Result<String, String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.token_at(now)
    }

    fn token_at(&self, now: u64) -> Result<String, String> {
        let mut cache = self.cache.lock().unwrap();
        if let Some((tok, exp)) = cache.as_ref()
            && now + 60 < *exp
        {
            return Ok(tok.clone());
        }
        let exp = now + 16 * 60;
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
        header.kid = Some(self.key_id.clone());
        let claims = Claims {
            iss: &self.issuer_id,
            iat: now,
            exp,
            aud: "appstoreconnect-v1",
        };
        let jwt = jsonwebtoken::encode(&header, &claims, &self.key).map_err(|e| e.to_string())?;
        *cache = Some((jwt.clone(), exp));
        Ok(jwt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // テスト専用に生成した P-256 鍵（どこにも登録していない）。
    const TEST_P8: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg0QDiWtbeViNiOUDu
2OHifcTNmdqs2vB63ITQeL7L5hChRANCAAQpfCDy7pRuexDD1gPko0Nq7caSWd7V
U36IWrZdpOdwDfv4/tb2O3v9zFPA2bQUlt5mBG04iHyEdkeU9WbT2E/F
-----END PRIVATE KEY-----
";

    #[test]
    fn mints_es256_with_kid_and_audience() {
        let ts = TokenSource::new("KEY123".into(), "issuer-1".into(), TEST_P8.as_bytes()).unwrap();
        let jwt = ts.token_at(1_700_000_000).unwrap();
        let header = jsonwebtoken::decode_header(&jwt).unwrap();
        assert_eq!(header.alg, jsonwebtoken::Algorithm::ES256);
        assert_eq!(header.kid.as_deref(), Some("KEY123"));
        let payload = jwt.split('.').nth(1).unwrap();
        use base64::Engine;
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(v["aud"], "appstoreconnect-v1");
        assert_eq!(v["iss"], "issuer-1");
        assert_eq!(v["exp"].as_u64().unwrap() - v["iat"].as_u64().unwrap(), 960);
        // ES256 の署名は R||S の 64 バイト
        let sig = jwt.split('.').nth(2).unwrap();
        assert_eq!(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(sig)
                .unwrap()
                .len(),
            64
        );
    }

    #[test]
    fn caches_until_a_minute_before_expiry() {
        let ts = TokenSource::new("K".into(), "I".into(), TEST_P8.as_bytes()).unwrap();
        let a = ts.token_at(1_000).unwrap();
        assert_eq!(ts.token_at(1_000 + 899).unwrap(), a);
        assert_ne!(ts.token_at(1_000 + 900).unwrap(), a);
    }

    #[test]
    fn rejects_non_pem() {
        assert!(TokenSource::new("K".into(), "I".into(), b"nope").is_err());
    }
}
