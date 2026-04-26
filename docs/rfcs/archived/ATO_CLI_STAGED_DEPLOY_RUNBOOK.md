# Ato CLI Staged Deploy Runbook (staging -> production)

`install.sh` (`apps/ato-store-web/install.sh`) を起点に、`ato` 配布物・Store API・Store Web を段階展開するための手順。

## 1. Scope

- CLI artifact: `ato-<target>.tar.gz` + `SHA256SUMS`
- R2 upload: `releases/<version>` + `latest`
- App deploy:
  - Store API (`apps/ato-store`)
  - Store Web (`apps/ato-store-web`)
- Verify:
  - `/install.sh` response
  - installer execution
  - API health
  - Web smoke E2E

## 2. Required scripts

- `/Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-cli/scripts/build_release_artifacts.sh`
- `/Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-cli/scripts/upload_release_to_r2.sh`
- `/Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-store/scripts/deploy_store_stack.sh`
- `/Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-store-web/scripts/verify_install_and_web.sh`

## 3. Release inputs

```bash
export VERSION="0.2.0"
export GIT_TAG="v${VERSION}"
export STAGING_RELEASE_BASE_URL="https://stg-dl.ato.run"
export PROD_RELEASE_BASE_URL="https://dl.ato.run"
export STAGING_R2_BUCKET="ato-store-artifacts-stg"
export PROD_R2_BUCKET="ato-store-artifacts-prod"
export TARGETS="x86_64-apple-darwin aarch64-apple-darwin x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu"
```

## 4. Build and package (manual multi-host)

Cross build is not used. Run on matching host per target.

### 4.1 On each host

```bash
cd /Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-cli
TARGETS="<single-target-for-this-host>" VERSION="${VERSION}" ./scripts/build_release_artifacts.sh
```

Artifacts are generated to:

- `/tmp/ato-release/${VERSION}/ato-<target>.tar.gz`
- `/tmp/ato-release/${VERSION}/SHA256SUMS`

### 4.2 Aggregate all targets on upload machine

Collect all `ato-<target>.tar.gz` into `/tmp/ato-release/${VERSION}` and regenerate checksum:

```bash
cd /tmp/ato-release/${VERSION}
shasum -a 256 ato-*.tar.gz > SHA256SUMS
```

## 5. Upload to staging (releases + latest)

```bash
cd /Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-cli
VERSION="${VERSION}" \
BUCKET="${STAGING_R2_BUCKET}" \
TARGETS="${TARGETS}" \
WRANGLER_CONFIG="/Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-store/wrangler.toml" \
./scripts/upload_release_to_r2.sh
```

## 6. Deploy staging API/Web

If migration diff check is required, set `BASE_REF`.

```bash
cd /Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-store
TARGET_ENV=staging \
BASE_REF="<staging_base_ref>" \
APPLY_MIGRATIONS=auto \
./scripts/deploy_store_stack.sh
```

## 7. Verify staging (required gate)

```bash
cd /Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-store-web
WEB_BASE_URL="https://staging.store.ato.run" \
API_BASE_URL="https://staging.api.ato.run" \
ATO_RELEASE_BASE_URL="${STAGING_RELEASE_BASE_URL}" \
PLAY_BASE_URL="https://staging.play.ato.run" \
PLAY_SANDBOX_BASE_DOMAIN="staging.atousercontent.com" \
./scripts/verify_install_and_web.sh
```

## 8. Promote to production (same artifacts)

```bash
cd /Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-cli
VERSION="${VERSION}" \
BUCKET="${PROD_R2_BUCKET}" \
TARGETS="${TARGETS}" \
WRANGLER_CONFIG="/Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-store/wrangler.toml" \
./scripts/upload_release_to_r2.sh
```

## 9. Deploy production API/Web

```bash
cd /Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-store
TARGET_ENV=production \
BASE_REF="<production_base_ref>" \
APPLY_MIGRATIONS=auto \
./scripts/deploy_store_stack.sh
```

## 10. Verify production (required gate)

```bash
cd /Users/egamikohsuke/Ekoh/projects/ato/capsuled-dev/apps/ato-store-web
WEB_BASE_URL="https://ato.run" \
API_BASE_URL="https://api.ato.run" \
ATO_RELEASE_BASE_URL="${PROD_RELEASE_BASE_URL}" \
PLAY_BASE_URL="https://play.ato.run" \
PLAY_SANDBOX_BASE_DOMAIN="atousercontent.com" \
./scripts/verify_install_and_web.sh
```

## 11. Rollback

1. CLI: re-upload previous version artifacts to `ato/latest/*`.
2. Web: deploy previous known-good commit (`deploy:staging` / `deploy:production`).
3. API: deploy previous known-good commit (`wrangler deploy --env <env>`).
4. DB: keep forward-only migration policy and recover with hotfix migration.

## 12. Acceptance checks

1. Every `ato-<target>.tar.gz` has only root `ato`.
2. `SHA256SUMS` matches generated archives.
3. `/install.sh` is reachable with `Content-Type: text/plain`.
4. staging install flow succeeds with `ATO_RELEASE_BASE_URL=https://stg-dl.ato.run`.
5. production install flow succeeds with default `https://dl.ato.run`.
6. API `/health` returns `200` on both staging and production.
7. Web smoke E2E passes on both staging and production.

## 13. Core Reliability Gates (Release N)

1. `ato build` (`runtime=source`, `.capsule` output) executes smoke test by default.
2. Smoke failure blocks build and removes artifact by default.
3. `--keep-failed-artifacts` keeps failed outputs for debug.
4. `--standalone` is temporarily outside mandatory smoke scope (warning only).
5. `ato run` default enforcement is `strict`.
6. Non-strict run requires explicit `--unsafe-bypass-sandbox`.
7. `ato install --skip-verify` is rejected; signature/hash verification is always enforced.
8. Legacy commands `ato open` / `ato pack` print deprecation warnings and delegate to `run` / `build`.
