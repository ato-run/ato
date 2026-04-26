#!/usr/bin/env bash
set -euo pipefail

# Upload ato release archives to an R2 bucket in both versioned and latest paths.
#
# Example:
#   VERSION="0.2.0" \
#   DEPLOY_ENV="staging" \
#   TARGETS="x86_64-apple-darwin aarch64-apple-darwin x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu" \
#   ./scripts/upload_release_to_r2.sh
#
# Env:
#   VERSION             required
#   BUCKET              optional (if omitted, inferred from DEPLOY_ENV)
#   DEPLOY_ENV          optional: staging|stg|production|prod
#   DEFAULT_BUCKET_STAGING     optional fallback bucket when DEPLOY_ENV=staging|stg
#   DEFAULT_BUCKET_PRODUCTION  optional fallback bucket when DEPLOY_ENV=production|prod
#   SOURCE_DIR          default: /tmp/ato-release/$VERSION
#   TARGETS             default: infer from SOURCE_DIR/<ASSET_PREFIX>-*.tar.xz|<ASSET_PREFIX>-*.tar.gz|<ASSET_PREFIX>-*.zip
#   ASSET_PREFIX        default: ato
#   UPDATE_LATEST       default: 1
#   PREFIX              default: ato
#   WRANGLER_CONFIG     optional (if empty, use wrangler default resolution)
#   WRANGLER_ENV        optional
#   REMOTE              default: 1 (set 0 to omit --remote)
#   DRY_RUN             default: 0
#   PUT_RETRIES         default: 5
#   PUT_RETRY_SLEEP     default: 2 (seconds; exponential backoff base)

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: required command not found: $1" >&2
    exit 1
  }
}

resolve_bucket_from_env() {
  case "${DEPLOY_ENV:-}" in
    staging|stg) echo "${DEFAULT_BUCKET_STAGING:-}" ;;
    production|prod) echo "${DEFAULT_BUCKET_PRODUCTION:-}" ;;
    *) echo "" ;;
  esac
}

resolve_wrangler_cmd() {
  if command -v wrangler >/dev/null 2>&1; then
    WRANGLER_CMD=(wrangler)
    return 0
  fi

  if command -v npx >/dev/null 2>&1; then
    WRANGLER_CMD=(npx --yes wrangler@4)
    return 0
  fi

  echo "error: required command not found: wrangler (or npx for fallback)" >&2
  exit 1
}

put_object() {
  local object_key="$1"
  local source_file="$2"
  local cache_control="${3:-}"
  local object_ref="${BUCKET}/${object_key}"
  local -a cmd
  local cmd_output

  cmd=("${WRANGLER_CMD[@]}")
  if [[ -n "$WRANGLER_CONFIG" ]]; then
    cmd+=(--config "$WRANGLER_CONFIG")
  fi
  if [[ -n "$WRANGLER_ENV" ]]; then
    cmd+=(--env "$WRANGLER_ENV")
  fi
  cmd+=(r2 object put "$object_ref" --file "$source_file")
  if [[ -n "$cache_control" ]]; then
    cmd+=(--cache-control "$cache_control")
  fi
  if [[ "$REMOTE" == "1" ]]; then
    cmd+=(--remote)
  fi

  if [[ "$DRY_RUN" == "1" ]]; then
    echo "[dry-run] ${cmd[*]}"
    return
  fi

  local attempt=1
  local max_attempts="$PUT_RETRIES"
  local sleep_base="$PUT_RETRY_SLEEP"
  local sleep_seconds
  while true; do
    if cmd_output="$("${cmd[@]}" 2>&1)"; then
      if [[ -n "$cmd_output" ]]; then
        echo "$cmd_output"
      fi
      return 0
    fi
    if [[ -n "$cmd_output" ]]; then
      echo "$cmd_output" >&2
    fi
    if grep -qiE 'specified bucket does not exist|bucket does not exist|authentication error|not authorized|permission denied|account id' <<<"$cmd_output"; then
      echo "error: non-retriable upload error for $object_ref. Verify BUCKET / DEFAULT_BUCKET_* and Cloudflare credentials." >&2
      return 1
    fi
    if (( attempt >= max_attempts )); then
      echo "error: upload failed after $attempt attempt(s): $object_ref" >&2
      return 1
    fi
    sleep_seconds=$(( sleep_base ** (attempt - 1) ))
    echo "warn: upload failed for $object_ref (attempt $attempt/$max_attempts), retrying in ${sleep_seconds}s..." >&2
    sleep "$sleep_seconds"
    attempt=$((attempt + 1))
  done
}

find_archive_for_target() {
  local target="$1"
  # Try cargo-dist format first: <ASSET_PREFIX>-<version>-<target>.tar.xz|tar.gz|zip

  # Find files matching the pattern for this target
  for file in "$SOURCE_DIR"/*; do
    if [[ -f "$file" ]]; then
      local filename="$(basename "$file")"
      # Check if it's a cargo-dist format file for this target
      if [[ "$filename" =~ ^${ASSET_PREFIX}-[^-]+-${target}\.(tar\.xz|tar\.gz|zip)$ ]]; then
        echo "$file"
        return 0
      fi
      # Also check old format for backward compatibility
      if [[ "$filename" == "${ASSET_PREFIX}-${target}.tar.xz" || "$filename" == "${ASSET_PREFIX}-${target}.tar.gz" || "$filename" == "${ASSET_PREFIX}-${target}.zip" ]]; then
        echo "$file"
        return 0
      fi
    fi
  done

  return 1
}

verify_checksums() {
  local checksum_file="$1"
  local source_dir="$2"

  if command -v sha256sum >/dev/null 2>&1; then
    (
      cd "$source_dir"
      sha256sum -c "$(basename "$checksum_file")"
    )
    return 0
  fi

  if ! command -v shasum >/dev/null 2>&1; then
    echo "error: no checksum verifier found (need sha256sum or shasum)" >&2
    exit 1
  fi

  (
    cd "$source_dir"
    while IFS= read -r line; do
      [[ -z "$line" ]] && continue

      expected_hash="$(awk '{print $1}' <<<"$line")"
      archive_name="$(awk '{print $2}' <<<"$line")"
      archive_name="${archive_name#\*}"

      if [[ -z "$expected_hash" || -z "$archive_name" ]]; then
        echo "error: invalid checksum entry: $line" >&2
        exit 1
      fi

      if [[ ! -f "$archive_name" ]]; then
        echo "error: checksum refers to missing file: $archive_name" >&2
        exit 1
      fi

      actual_hash="$(shasum -a 256 "$archive_name" | awk '{print $1}')"
      if [[ "$actual_hash" != "$expected_hash" ]]; then
        echo "error: checksum mismatch for $archive_name" >&2
        exit 1
      fi
    done < "$(basename "$checksum_file")"
  )
}

need_cmd find
resolve_wrangler_cmd

VERSION="${VERSION:-}"
DEPLOY_ENV="${DEPLOY_ENV:-}"
DEFAULT_BUCKET_STAGING="${DEFAULT_BUCKET_STAGING:-}"
DEFAULT_BUCKET_PRODUCTION="${DEFAULT_BUCKET_PRODUCTION:-}"
BUCKET="${BUCKET:-$(resolve_bucket_from_env)}"
if [[ -z "$VERSION" ]]; then
  echo "error: VERSION is required" >&2
  exit 1
fi
if [[ -z "$BUCKET" ]]; then
  echo "error: BUCKET is required (or set DEPLOY_ENV plus DEFAULT_BUCKET_STAGING/DEFAULT_BUCKET_PRODUCTION)" >&2
  exit 1
fi

SOURCE_DIR="${SOURCE_DIR:-/tmp/ato-release/${VERSION}}"
TARGETS="${TARGETS:-}"
ASSET_PREFIX="${ASSET_PREFIX:-ato}"
UPDATE_LATEST="${UPDATE_LATEST:-1}"
PREFIX="${PREFIX:-ato}"
WRANGLER_CONFIG="${WRANGLER_CONFIG:-}"
WRANGLER_ENV="${WRANGLER_ENV:-}"
REMOTE="${REMOTE:-1}"
DRY_RUN="${DRY_RUN:-0}"
PUT_RETRIES="${PUT_RETRIES:-5}"
PUT_RETRY_SLEEP="${PUT_RETRY_SLEEP:-2}"
RELEASE_CACHE_CONTROL="${RELEASE_CACHE_CONTROL:-public, max-age=31536000, immutable}"
LATEST_CACHE_CONTROL="${LATEST_CACHE_CONTROL:-no-store, max-age=0}"

if [[ -n "$WRANGLER_CONFIG" && ! -f "$WRANGLER_CONFIG" ]]; then
  echo "error: WRANGLER_CONFIG not found: $WRANGLER_CONFIG" >&2
  exit 1
fi

if [[ ! -d "$SOURCE_DIR" ]]; then
  echo "error: SOURCE_DIR not found: $SOURCE_DIR" >&2
  exit 1
fi

target_list=()
if [[ -n "$TARGETS" ]]; then
  read -r -a target_list <<<"$TARGETS"
else
  while IFS= read -r archive; do
    archive_name="$(basename "$archive")"
    # Handle both old format (<ASSET_PREFIX>-<target>.<ext>) and new cargo-dist format (<ASSET_PREFIX>-<version>-<target>.<ext>)
    if [[ "$archive_name" =~ ^${ASSET_PREFIX}-[^-]+-(.+)\.(tar\.xz|tar\.gz|zip)$ ]]; then
      # cargo-dist format: <ASSET_PREFIX>-<version>-<target>.<ext>
      target="${BASH_REMATCH[1]}"
    else
      # old format: <ASSET_PREFIX>-<target>.<ext>
      target="${archive_name#${ASSET_PREFIX}-}"
      target="${target%.tar.xz}"
      target="${target%.tar.gz}"
      target="${target%.zip}"
    fi
    target_list+=("$target")
  done < <(find "$SOURCE_DIR" -maxdepth 1 -type f \( -name "${ASSET_PREFIX}-*.tar.xz" -o -name "${ASSET_PREFIX}-*.tar.gz" -o -name "${ASSET_PREFIX}-*.zip" \) | sort)
fi

if [[ "${#target_list[@]}" -eq 0 ]]; then
  echo "error: no targets detected. Set TARGETS or place ${ASSET_PREFIX}-<target>.tar.xz / ${ASSET_PREFIX}-<target>.tar.gz / ${ASSET_PREFIX}-<target>.zip in $SOURCE_DIR" >&2
  exit 1
fi

deduped_targets=()
while IFS= read -r target; do
  deduped_targets+=("$target")
done < <(printf '%s\n' "${target_list[@]}" | sort -u)
target_list=("${deduped_targets[@]}")

checksum_file="$SOURCE_DIR/SHA256SUMS"
if [[ ! -f "$checksum_file" ]]; then
  echo "error: SHA256SUMS not found: $checksum_file" >&2
  exit 1
fi

for target in "${target_list[@]}"; do
  if ! archive_file="$(find_archive_for_target "$target")"; then
    echo "error: archive not found for target '$target': $SOURCE_DIR/${ASSET_PREFIX}-$target.tar.xz|.tar.gz|.zip" >&2
    exit 1
  fi
  archive_name="$(basename "$archive_file")"
  if ! grep -qE "^[[:xdigit:]]{64}[[:space:]]+\*?${archive_name}$" "$checksum_file"; then
    echo "error: SHA256SUMS missing entry for $archive_name" >&2
    exit 1
  fi
done

verify_checksums "$checksum_file" "$SOURCE_DIR"

for target in "${target_list[@]}"; do
  if ! archive_file="$(find_archive_for_target "$target")"; then
    echo "error: archive not found for target '$target': $SOURCE_DIR/${ASSET_PREFIX}-$target.tar.xz|.tar.gz|.zip" >&2
    exit 1
  fi
  archive_name="$(basename "$archive_file")"

  put_object "$PREFIX/releases/$VERSION/$archive_name" "$archive_file" "$RELEASE_CACHE_CONTROL"

  if [[ "$UPDATE_LATEST" == "1" ]]; then
    put_object "$PREFIX/latest/$archive_name" "$archive_file" "$LATEST_CACHE_CONTROL"
  fi
done

put_object "$PREFIX/releases/$VERSION/SHA256SUMS" "$checksum_file" "$RELEASE_CACHE_CONTROL"
if [[ "$UPDATE_LATEST" == "1" ]]; then
  put_object "$PREFIX/latest/SHA256SUMS" "$checksum_file" "$LATEST_CACHE_CONTROL"
fi

echo "==> upload completed"
echo "    bucket : $BUCKET"
echo "    env    : ${DEPLOY_ENV:-<manual>}"
echo "    version: $VERSION"
echo "    prefix : $PREFIX"
echo "    targets: ${target_list[*]}"
echo "    latest : $UPDATE_LATEST"
echo "    remote : $REMOTE"
echo "    config : ${WRANGLER_CONFIG:-<wrangler default>}"
echo "    cache(versioned): $RELEASE_CACHE_CONTROL"
echo "    cache(latest)   : $LATEST_CACHE_CONTROL"
