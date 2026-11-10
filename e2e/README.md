# E2E（テストアプリ）

releasectl の apply を実ストアで検証するための、審査に出さないテストアプリ。
**apply を流してよいのはこのアプリだけ**（sonir などの本物のアプリには plan しか流さない）。

| ストア | 識別子 | 状態 |
|---|---|---|
| App Store Connect | bundle id `dev.releasectl.e2e`、アプリ名 `releasectl-e2e`、SKU `releasectl-e2e` | 作成済み（2026-11-09）。グループ `E2E Pro`、サブスク 3 件・買い切り 1 件を作成済み |
| Google Play | package `dev.releasectl.e2e`、アプリ名 `releasectl-e2e`（無料） | 作成済み（2026-11-09）。内部テストに AAB（versionCode 1）を下書きで登録済み。サブスク 3 件・買い切り 1 件を作成済み |

- `metadata.yaml` — テストアプリの掲載情報。画像は `python3 make_shots.py [VARIANT]` で `shots/` に作る（リポジトリには入れない）。
  年齢区分は Console で質問票に答えるまで `~ ageRating` が出続ける。
- `catalog.yaml` — テストアプリのカタログ。**App Store の productId は一度作ると消せず再利用もできない**ので、商品を足すときは新しい id にする。
- `app/` — Play に上げる空の Flutter アプリ（BILLING 権限だけ入れてある）。`make e2e-aab` でビルドする。
  アップロード鍵は `~/.releasectl-secrets/e2e-upload.{jks,properties}`（リポジトリの外。無くすと Play のアップロード鍵のリセットが要る）。

```bash
make e2e-plan  ENV_FILE=~/sonir/sonir-workspace/releasectl/.env
make e2e-apply ENV_FILE=~/sonir/sonir-workspace/releasectl/.env
```
