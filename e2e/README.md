# E2E test app

A test app that is never submitted for review, used to check releasectl's writes against the real stores.
**This is the only app `apply` is run against.** Real apps only ever get `plan`.

| Store | Identifiers |
|---|---|
| App Store Connect | bundle ID `dev.releasectl.e2e`, app name `releasectl-e2e`. Subscription group `E2E Pro`, three subscriptions and one non-consumable |
| Google Play | package `dev.releasectl.e2e`, app name `releasectl-e2e` (free). An AAB is registered on the internal track as a draft. Three subscriptions and one one-time product |

- `catalog.yaml` — the test app's purchase catalog. **App Store product IDs can never be deleted or reused**, so use a new ID when you add a product.
- `metadata.yaml` — the test app's store listing. Generate the images into `shots/` with `python3 make_shots.py [VARIANT]` (they are not committed).
  `~ ageRating` keeps showing until the age rating questionnaire is answered once in App Store Connect.
- `app/` — an empty Flutter app for Google Play, with only the BILLING permission. Build it with `make e2e-aab`.
  The upload key lives outside the repository in `~/.releasectl-secrets/e2e-upload.{jks,properties}`. Losing it means resetting the upload key in Play Console.

```bash
make e2e-plan  ENV_FILE=~/secrets/releasectl.env    # a file of KEY=value lines with the store keys
make e2e-apply ENV_FILE=~/secrets/releasectl.env
```

The workflows in `.github/workflows/` run the same checks: a plan on pull requests that touch these files, an apply on merge, and a drift check every day.
