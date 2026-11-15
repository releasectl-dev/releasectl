# releasectl

Manage your App Store Connect and Google Play listings as code.

releasectl is a CLI that reads two YAML files and brings both stores in line with them:

- `iap.yaml` — the purchase catalog: auto-renewable subscriptions and one-time products, their prices, availability and localized names.
- `metadata.yaml` — the store listing: per-locale text, review information, app settings, screenshots and creatives.

`plan` shows what differs and writes nothing. `apply` creates what is missing and fixes what has drifted. Both are idempotent, so they fit pull requests and scheduled runs: review the plan on the PR, apply on merge, and get an Issue when someone changes a price in the console by hand.

```text
$ releasectl iap plan
PLAN — no changes will be made ("MyApp")

=== appstore ===
  = subscription         app.myapp.monthly
  ~ price                app.myapp.monthly  — USD 4.99→USD 5.99
  + localization         app.myapp.monthly [ja]

=== googleplay ===
  = subscription         myapp_monthly
  ~ basePlan.price       monthly  — USD 4.99→USD 5.99

plan only — run `releasectl iap apply` to create the missing resources.
```

`+` to create · `~` drift (exists but differs) · `✓` created / fixed · `=` in sync · `-` skipped · `✗` error

## Your store keys stay with you

The App Store Connect API key and the Google Play service account can change everything you sell. releasectl reads them from environment variables and sends them **only to Apple's and Google's APIs** — nowhere else. There is no releasectl server in the loop.

The code is split so this is easy to check:

| Crate | What it does | Keys / network |
|---|---|---|
| `crates/core` (`releasectl-core`) | Parses and validates the YAML files, defines the plan / apply report | Never touches either |
| `crates/stores` (`releasectl-stores`) | Talks to App Store Connect and Google Play, and signs the requests | The only crate that does |
| `crates/cli` (`releasectl`) | Commands and output | — |

## Install

releasectl is a single binary. Build it from a tagged release with Cargo (Rust 1.85 or newer):

```bash
cargo install --locked --git https://github.com/releasectl-dev/releasectl --tag v0.9.1 releasectl
releasectl --version
```

## Credentials

Set the variables for the stores you use. A store without credentials is skipped and reported as such, so you can start with one.

| Store | Variables |
|---|---|
| App Store | `ASC_KEY_ID`, `ASC_ISSUER_ID`, and `ASC_PRIVATE_KEY` (the `.p8` contents) or `ASC_PRIVATE_KEY_PATH` |
| Google Play | `GOOGLE_PLAY_SERVICE_ACCOUNT_JSON` (the JSON itself), or `GOOGLE_PLAY_SERVICE_ACCOUNT_PATH`, or `GOOGLE_APPLICATION_CREDENTIALS` |

## Quick start

Already have an app in the stores? Let releasectl write the files from what is there. `init` only reads.

```bash
releasectl iap init  --bundle-id app.myapp --package-name app.myapp   # writes iap.yaml
releasectl meta init --bundle-id app.myapp --package-name app.myapp   # writes metadata.yaml and the images
releasectl iap plan                                                   # should report no changes
```

Starting from scratch, write `iap.yaml` by hand:

```yaml
appName: MyApp
bundleId: app.myapp          # App Store bundle ID
packageName: app.myapp       # Google Play package name

pricing:
  territory: USA             # App Store territory
  region: US                 # Google Play region
  currency: USD

products:
  - key: monthly
    appStoreId: app.myapp.monthly
    playProductId: myapp_monthly
    playBasePlanId: monthly
    period: P1M
    price: 4.99
    localizations:
      - locale: en-US
        name: "Pro Monthly"
        description: "Everything in Pro, billed monthly."
```

Then:

```bash
releasectl iap plan     # show the differences; writes nothing
releasectl iap apply    # create what is missing, fix what drifted
releasectl iap plan     # everything is "=" now
```

> **App Store product IDs can never be deleted or reused.** Run `plan` first, and try `apply` on a test app before you point it at a live one.

## Commands

| Command | Writes to the stores | What it does |
|---|---|---|
| `iap plan` / `iap apply` | apply only | Subscriptions and one-time products from `iap.yaml` |
| `iap retire-plan` / `iap retire` | retire only | Takes the configured products off sale. Nothing is deleted |
| `iap init` | No | Writes `iap.yaml` from the products already in the stores |
| `meta plan` / `meta apply` | apply only | The store listing from `metadata.yaml`. Never submits for review |
| `meta init` | No | Writes `metadata.yaml` and the images from the current listing |
| `meta export` | No | Prints the current listing as JSON |
| `play upload-plan` / `play upload` | upload only | Uploads an Android App Bundle to a track |

Useful flags on `plan` and `apply`:

```bash
releasectl iap plan --config path/to/iap.yaml --only googleplay
releasectl iap plan -o json --detailed-exitcode    # for CI: exit 2 when there are changes
releasectl iap plan -o markdown                    # for pull request comments
releasectl meta apply --app-store-version 1.4.0 --create-app-store-version
```

Run `releasectl <command> --help` for the full list.

## GitHub Actions

The repository is also a GitHub Action. It comments the plan on the pull request and applies on merge:

```yaml
on:
  pull_request:
    paths: [iap.yaml]
  push:
    branches: [main]
    paths: [iap.yaml]

permissions:
  contents: read
  pull-requests: write

jobs:
  releasectl:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: releasectl-dev/releasectl@v0.9.1
        with:
          command: ${{ github.event_name == 'pull_request' && 'iap-plan' || 'iap-apply' }}
          config: iap.yaml
        env:   # keys come from your repository secrets and go only to the stores
          ASC_KEY_ID: ${{ secrets.ASC_KEY_ID }}
          ASC_ISSUER_ID: ${{ secrets.ASC_ISSUER_ID }}
          ASC_PRIVATE_KEY: ${{ secrets.ASC_PRIVATE_KEY }}
          GOOGLE_PLAY_SERVICE_ACCOUNT_JSON: ${{ secrets.GOOGLE_PLAY_SERVICE_ACCOUNT_JSON }}
```

| Input | Default | |
|---|---|---|
| `command` | `iap-plan` | `iap-plan`, `iap-apply`, `iap-retire-plan`, `iap-retire`, `meta-plan`, `meta-apply`, `meta-export` |
| `config` | `iap.yaml` | Path to the YAML file |
| `only` | all | `appstore`, `googleplay` |
| `drift-issue` | `false` | On a scheduled `*-plan`, open an Issue when the store drifts and close it when it is back in sync |
| `app-store-version` / `create-app-store-version` | — | For `meta-*`: the App Store version to edit, and whether to create it |

Outputs: `exitcode` (`0` no changes, `2` changes, `1` errors), `has-changes`, and `json` (path to the JSON report).

This repository runs the Action on itself: `.github/workflows/` keeps a test app's catalog and listing in sync and checks it for drift every day.

## What it does not do

- Submit anything for review, or attach builds to an App Store version.
- Delete products. `retire` takes them off sale; `apply` puts them back.
- Manage RevenueCat or any other billing backend.
- Set more than one price per product: one territory on the App Store and one region on Google Play. On Google Play, other regions are kept as they are. On the App Store, changing the price of a one-time purchase replaces its whole price schedule, so manual prices you set for other territories in App Store Connect are removed for that product.

## Development

```bash
make build    # target/release/releasectl
make test     # cargo test
make lint     # cargo fmt --check + clippy -D warnings
```

Changes to how releasectl reads or writes a store are checked against a test app (`e2e/`), the only app `apply` is ever run against in this repository. See `e2e/README.md`.

## License

[Apache-2.0](LICENSE)
