# releasectl

App Store Connect / Google Play の課金カタログ（自動更新サブスク + 買い切り）を、1 枚の
`iap.yaml` から `plan` / `apply` で reconcile する CLI。sonir の Go 版 releasectl
（`iapctl`）を Rust に書き直したもの。仕様の正本はワークスペースの `docs/02_cli-architecture/`。

```bash
make build
target/release/releasectl iap plan  --config iap.yaml                 # 差分の表示だけ（書き込みなし）
target/release/releasectl iap apply --config iap.yaml                 # 足りないものを作る（冪等）
target/release/releasectl iap plan  --config iap.yaml --only googleplay
target/release/releasectl iap retire-plan --config iap.yaml             # 販売停止の差分（書き込みなし）
target/release/releasectl play upload-plan --package <id> --aab app.aab   # .aab を上げて検証し、捨てる
```

- **鍵は環境変数から読み、送る先は各ストアの API だけ。** 鍵の env が無い provider は飛ばす。
- RevenueCat は扱わない（2026-11-09）。Go 版の iap.yaml にある `entitlement` / `revenuecat` /
  `rcPackage` は読み飛ばすので、既存のファイルをそのまま使える。

| Provider | 変数 |
|---|---|
| Google Play | `GOOGLE_PLAY_SERVICE_ACCOUNT_JSON`（インライン）または `GOOGLE_PLAY_SERVICE_ACCOUNT_PATH` または `GOOGLE_APPLICATION_CREDENTIALS` |
| App Store | `ASC_KEY_ID`、`ASC_ISSUER_ID`、`ASC_PRIVATE_KEY` または `ASC_PRIVATE_KEY_PATH` |

## 構成

| crate | 内容 | 鍵 / ネットワーク |
|---|---|---|
| `crates/core`（`releasectl-core`） | iap.yaml の型と検証、`Action` / `Status` | 触れない（Cloud の wasm からも使う） |
| `crates/stores`（`releasectl-stores`） | ストアの reconciler（`googleplay` / `appstore`）、HTTP、各ストアの認証 | 触れる |
| `crates/cli`（`releasectl`） | コマンドと表示 | — |

## 開発

```bash
make test     # cargo test
make lint     # cargo fmt --check + clippy -D warnings
make parity   # Go 版との plan 出力の一致（GO_SRC / IAP_YAML / ENV_FILE が要る）
```

### パリティテスト

Rust 版の `plan` は Go 版 `iapctl plan` と出力が一致することが契約（移植が終わるまで）。
`parity/run.sh` は実物の iap.yaml から 4 ケース（そのまま / 全価格 +1 / 存在しない商品を追加 /
説明文を 1 つ変更）を作り、両方で `plan` を流して diff する。plan は書き込みをしないので本番の
ストアに対して流してよい。ケースと結果はリポジトリの外（`../scratch/parity`）に置く。

意図的な差分（正規化して比べる）:

- コマンド名: `iapctl apply` / `iapctl retire` → `releasectl iap apply` / `releasectl iap retire`
