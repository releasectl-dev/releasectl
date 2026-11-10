//! JSON over HTTP の小さなヘルパー（Go 版 `internal/httpx`）。
//! 毎リクエストで Authorization を付け直し、429 は Retry-After に従って再試行する。

use std::time::Duration;

pub type AuthFn = Box<dyn Fn() -> Result<String, String>>;

pub struct Client {
    /// JSON の API 用（30 秒）
    http: reqwest::blocking::Client,
    /// 大きなファイルのアップロード（do_raw）用。数十 MB の .aab や動画を送り切れるよう長くする
    upload: reqwest::blocking::Client,
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
        // 接続の確立で詰まったら早く諦める（全体のタイムアウトはアップロードのために長いことがある）
        .connect_timeout(Duration::from_secs(30))
        .build()
        .expect("build http client")
}

impl Client {
    pub fn new(http: reqwest::blocking::Client, base: &str, auth: Option<AuthFn>) -> Self {
        Client {
            http,
            upload: http_client_with_timeout(Duration::from_secs(15 * 60)),
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
            let resp = match req.send() {
                Ok(resp) => resp,
                // 通信エラー（タイムアウトなど）も、GET だけは 1 / 2 / 4 秒で再試行する
                Err(_) if method == reqwest::Method::GET && attempt < 3 => {
                    std::thread::sleep(Duration::from_secs(1 << attempt));
                    attempt += 1;
                    continue;
                }
                Err(e) => return Err(e.to_string()),
            };
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
            // 5xx は GET だけ再試行する（1 秒・2 秒・4 秒）。書き込みは二重に作らないよう再試行しない。
            if (500..600).contains(&status) && method == reqwest::Method::GET && attempt < 3 {
                std::thread::sleep(Duration::from_secs(1 << attempt));
                attempt += 1;
                continue;
            }
            return Ok(Response { status, body });
        }
    }
}

impl Client {
    /// 生のバイト列のアップロード（Go 版 `DoRaw`）。JSON にせず、Content-Type を明示する。
    /// `with_auth` が false なら Authorization を付けない（App Store の予約済みアップロード URL は
    /// Authorization を拒む。Google Play のメディアアップロードは要る）。`extra_headers` はそのまま送る
    /// （App Store の予約が返す part ごとのヘッダー）。429 は Retry-After に従う。
    pub fn do_raw(
        &self,
        method: &str,
        url: &str,
        body: &[u8],
        content_type: &str,
        with_auth: bool,
        extra_headers: &[(String, String)],
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
                .upload
                .request(method.clone(), &full)
                .body(body.to_vec());
            if !content_type.is_empty() {
                req = req.header("Content-Type", content_type);
            }
            for (k, v) in extra_headers {
                req = req.header(k.as_str(), v.as_str());
            }
            if with_auth && let Some(auth) = &self.auth {
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
