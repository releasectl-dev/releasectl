//! JSON over HTTP の小さなヘルパー（Go 版 `internal/httpx`）。
//! 毎リクエストで Authorization を付け直し、429 は Retry-After に従って再試行する。

use std::time::Duration;

pub type AuthFn = Box<dyn Fn() -> Result<String, String>>;

pub struct Client {
    http: reqwest::blocking::Client,
    base: String,
    /// Authorization ヘッダの値全体（"Bearer xyz"）を返す。
    auth: Option<AuthFn>,
}

pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Response {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn json<T: serde::de::DeserializeOwned + Default>(&self) -> Result<T, String> {
        if self.body.is_empty() {
            return Ok(T::default());
        }
        serde_json::from_slice(&self.body).map_err(|e| e.to_string())
    }

    /// ステータスと切り詰めた本文（Go 版 `Response.Error`）。
    pub fn error(&self) -> String {
        let b = String::from_utf8_lossy(&self.body);
        // Go はバイト数で切る。UTF-8 の途中で切らないよう文字境界まで戻す。
        let b = if b.len() > 600 {
            let mut end = 600;
            while !b.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}…", &b[..end])
        } else {
            b.into_owned()
        };
        format!("HTTP {}: {}", self.status, b)
    }
}

pub fn http_client() -> reqwest::blocking::Client {
    http_client_with_timeout(Duration::from_secs(30))
}

/// 大きなアップロード（数十 MB の .aab）用に、タイムアウトを変えたクライアント。
pub fn http_client_with_timeout(timeout: Duration) -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .expect("build http client")
}

impl Client {
    pub fn new(http: reqwest::blocking::Client, base: &str, auth: Option<AuthFn>) -> Self {
        Client {
            http,
            base: base.to_string(),
            auth,
        }
    }

    /// method で path（base に連結。絶対 URL ならそのまま）を叩く。body は JSON にする。
    pub fn r#do(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<Response, String> {
        let url = if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else {
            format!("{}{}", self.base, path)
        };
        let raw = body.map(|b| serde_json::to_vec(b).expect("json body"));
        let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;

        let mut attempt = 0;
        loop {
            let mut req = self.http.request(method.clone(), &url);
            if let Some(raw) = &raw {
                req = req
                    .header("Content-Type", "application/json")
                    .body(raw.clone());
            } else if method != reqwest::Method::GET && method != reqwest::Method::DELETE {
                // 本文の無い POST（edits:commit など）にも Content-Length: 0 を付ける。
                // 付けないと Google のフロントエンドが 411 Length Required を返す（Go は自動で付ける）。
                req = req.body(Vec::new());
            }
            if let Some(auth) = &self.auth {
                let v = auth().map_err(|e| format!("auth: {e}"))?;
                req = req.header("Authorization", v);
            }
            let resp = req.send().map_err(|e| e.to_string())?;
            let status = resp.status().as_u16();
            let retry = resp
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .map(retry_after);
            let body = resp.bytes().map_err(|e| e.to_string())?.to_vec();
            if status == 429 && attempt < 3 {
                std::thread::sleep(retry.unwrap_or(Duration::from_secs(2)));
                attempt += 1;
                continue;
            }
            return Ok(Response { status, body });
        }
    }
}

impl Client {
    /// 生のバイト列のアップロード（Go 版 `DoRaw`）。JSON にせず、Content-Type を明示する。
    /// Google Play のメディアアップロードは Authorization が要る。429 は Retry-After に従う。
    pub fn do_raw(
        &self,
        method: &str,
        url: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<Response, String> {
        let full = if url.starts_with("http://") || url.starts_with("https://") {
            url.to_string()
        } else {
            format!("{}{}", self.base, url)
        };
        let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
        let mut attempt = 0;
        loop {
            let mut req = self
                .http
                .request(method.clone(), &full)
                .header("Content-Type", content_type)
                .body(body.clone());
            if let Some(auth) = &self.auth {
                let v = auth().map_err(|e| format!("auth: {e}"))?;
                req = req.header("Authorization", v);
            }
            let resp = req.send().map_err(|e| e.to_string())?;
            let status = resp.status().as_u16();
            let retry = resp
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .map(retry_after);
            let body = resp.bytes().map_err(|e| e.to_string())?.to_vec();
            if status == 429 && attempt < 3 {
                std::thread::sleep(retry.unwrap_or(Duration::from_secs(2)));
                attempt += 1;
                continue;
            }
            return Ok(Response { status, body });
        }
    }
}

fn retry_after(h: &str) -> Duration {
    match h.trim().parse::<u64>() {
        Ok(secs) if secs > 0 => Duration::from_secs(secs),
        _ => Duration::from_secs(2),
    }
}
