.PHONY: build test lint fmt parity e2e-plan e2e-apply e2e-aab

build: ## target/release/releasectl
	cargo build --release

test: ## cargo test
	cargo test

lint: ## fmt の差分なし + clippy -D warnings
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings

fmt:
	cargo fmt

## Go 版 iapctl との plan 出力の一致テスト（読み取り専用。本番のストアに流してよい）。
## GO_SRC / IAP_YAML / ENV_FILE を渡す。ワークスペースの `make parity` が sonir の値で呼ぶ。
parity:
	./parity/run.sh

## --- E2E（テストアプリ dev.releasectl.e2e。apply を流してよい唯一の対象）---
## ENV_FILE で鍵の env を渡す（例: ENV_FILE=~/sonir/sonir-workspace/releasectl/.env）
ENV_PATH = $(patsubst ~/%,$(HOME)/%,$(ENV_FILE))
E2E_RUN = set -a; [ -n "$(ENV_PATH)" ] && . "$(ENV_PATH)"; set +a;

e2e-plan: build ## テストアプリに plan
	@$(E2E_RUN) cd $(dir $(ENV_PATH)). && $(CURDIR)/target/release/releasectl iap plan --config $(CURDIR)/e2e/catalog.yaml

e2e-apply: build ## テストアプリに apply（作った商品は消せない）
	@$(E2E_RUN) cd $(dir $(ENV_PATH)). && $(CURDIR)/target/release/releasectl iap apply --config $(CURDIR)/e2e/catalog.yaml

e2e-aab: ## Play に上げる空の AAB（BILLING 権限入り）。鍵は ~/.releasectl-secrets/e2e-upload.properties
	cd e2e/app && flutter build appbundle --release
