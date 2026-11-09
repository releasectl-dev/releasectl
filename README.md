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
target/release/releasectl iap plan --config iap.yaml -o json --detailed-exitcode   # CI 向け（差分ありで exit 2）
```

出力の記号: `+` 作成予定 / `~` ドリフト（あるが違う）/ `✓` 作った・直した / `=` 一致 / `-` 飛ばした / `✗` エラー。

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

### 回帰の確かめ方

- `make test`（ユニットテスト）と、テストアプリ（`e2e/`）への `make e2e-plan` / `e2e-apply`。
- Go 版とのパリティ（`parity/run.sh`）は v0.1.0 で完了した。Phase 0（v0.2.0）から出力が変わったので、
  流すなら v0.1.0 をチェックアウトする。
