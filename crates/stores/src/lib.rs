//! ストアの reconciler。鍵（.p8 / サービスアカウント JSON）に触れるのはこの crate
//! だけで、鍵は環境変数から読み、送る先は各ストアの API だけ。

pub mod appstore;
pub mod googleplay;
pub mod http;
pub mod meta;

use releasectl_core::{Config, Report};

/// 1 つのストアの設定を desired-state に近づける。
///
/// 冪等であること: 先に既存のリソースを list し、足りないものだけを作る。
/// `apply = false` のときは書き込みを一切せず、何をするかだけを報告する。
pub trait Reconciler {
    fn name(&self) -> &'static str;
    /// 返す `Err` は provider 全体を止める致命的なエラー（Go 版の `fatal`）。
    /// 項目ごとの失敗は `Status::Error` として report に積む。
    fn reconcile(&self, apply: bool, report: &mut Report) -> Result<(), String>;
    /// 設定した商品を販売停止にする（消さない。apply で戻せる）。`apply = false` は書き込みなし。
    /// 販売停止の API が無いストアは、手作業の案内を `Status::Skip` で報告する。
    fn retire(&self, _apply: bool, _report: &mut Report) -> Result<(), String> {
        Err(format!("{}: retire is not supported here", self.name()))
    }
    /// ストアの今の値を JSON で返す（読むだけ。`releasectl meta export`）。項目ごとの読み取りの失敗は
    /// `Status::Error` として report に積み、その値は出さない（「無い」とはみなさない）。
    fn export(
        &self,
        _opts: &ExportOpts,
        _report: &mut Report,
    ) -> Result<serde_json::Value, String> {
        Err(format!("{}: export is not supported here", self.name()))
    }
}

/// `export` に足すもの。既定（`meta export`）は何も足さない。`meta init` が両方を使う。
#[derive(Debug, Clone, Default)]
pub struct ExportOpts {
    /// 画像をこのディレクトリにダウンロードし、置いたファイルを `screenshotFiles` / `imageFiles` に出す
    pub images: Option<std::path::PathBuf>,
    /// App Store の審査情報（連絡先・デモアカウント）を `review` に出す。公開されていない情報なので
    /// `meta export`（Cloud に送る）では出さない。パスワードはどちらでも出さない
    pub review: bool,
}

/// 鍵が無いときは `Err(理由)` を返し、呼び出し側はその provider を飛ばす。
pub type Builder = fn(&Config) -> Result<Box<dyn Reconciler>, String>;

/// 実行順に並べた provider。
pub const PROVIDERS: &[(&str, Builder)] = &[
    ("appstore", appstore::build),
    ("googleplay", googleplay::build),
];

/// 空でない環境変数。
pub(crate) fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Go の `os.ReadFile` のエラー文（"open <path>: no such file or directory"）に寄せる。
pub(crate) fn go_path_err(path: &str, e: &std::io::Error) -> String {
    let msg = match e.kind() {
        std::io::ErrorKind::NotFound => "no such file or directory".to_string(),
        std::io::ErrorKind::PermissionDenied => "permission denied".to_string(),
        _ => e.to_string(),
    };
    format!("open {path}: {msg}")
}
