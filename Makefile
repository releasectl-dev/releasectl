.PHONY: build test lint fmt e2e-plan e2e-apply e2e-aab

build: ## target/release/releasectl
	cargo build --release

test: ## cargo test
	cargo test

lint: ## cargo fmt --check + clippy -D warnings
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings

fmt:
	cargo fmt

## --- E2E (the test app dev.releasectl.e2e, the only app apply is ever run against) ---
## Pass the store keys with ENV_FILE, a file of KEY=value lines (e.g. ENV_FILE=~/secrets/releasectl.env).
ENV_PATH = $(patsubst ~/%,$(HOME)/%,$(ENV_FILE))
E2E_RUN = set -a; [ -n "$(ENV_PATH)" ] && . "$(ENV_PATH)"; set +a;

e2e-plan: build ## plan against the test app (exit 0 when in sync, 2 when there are changes)
	@$(E2E_RUN) cd $(dir $(ENV_PATH)). && $(CURDIR)/target/release/releasectl iap plan --config $(CURDIR)/e2e/catalog.yaml --detailed-exitcode

e2e-apply: build ## apply to the test app (products it creates can never be deleted)
	@$(E2E_RUN) cd $(dir $(ENV_PATH)). && $(CURDIR)/target/release/releasectl iap apply --config $(CURDIR)/e2e/catalog.yaml

e2e-aab: ## build the empty AAB for Google Play (with the BILLING permission). Upload key: ~/.releasectl-secrets/e2e-upload.properties
	cd e2e/app && flutter build appbundle --release
