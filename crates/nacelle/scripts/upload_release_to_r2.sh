#!/usr/bin/env bash
set -euo pipefail

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

put_object() {
  local object_key="$1"
  local source_file="$2"
  local cache_control="${3:-}"
  local object_ref="${BUCKET}/${object_key}"
  local -a cmd

  cmd=(wrangler)
  if [[ -n "${WRANGLER_CONFIG:-}" ]]; then
    cmd+=(--config "$WRANGLER_CONFIG")
  fi
  if [[ -n "${WRANGLER_ENV:-}" ]]; then
    cmd+=(--env "$WRANGLER_ENV")
  fi
  cmd+=(r2 object put "$object_ref" --file "$source_file")
  if [[ -n "$cache_control" ]]; then
    cmd+=(--cache-control "$cache_control")
  fi
  if [[ "${REMOTE:-1}" == "1" ]]; then
    cmd+=(--remote)
  fi

  "${cmd[@]}"
}

need_cmd find
need_cmd wrangler

VERSION="${VERSION:-}"
DEPLOY_ENV="${DEPLOY_ENV:-}"
DEFAULT_BUCKET_STAGING="${DEFAULT_BUCKET_STAGING:-}"
DEFAULT_BUCKET_PRODUCTION="${DEFAULT_BUCKET_PRODUCTION:-}"
BUCKET="${BUCKET:-$(resolve_bucket_from_env)}"
SOURCE_DIR="${SOURCE_DIR:-/tmp/nacelle-release/${VERSION}}"
PREFIX="${PREFIX:-nacelle}"
UPDATE_LATEST="${UPDATE_LATEST:-1}"
WRANGLER_CONFIG="${WRANGLER_CONFIG:-}"
WRANGLER_ENV="${WRANGLER_ENV:-}"
REMOTE="${REMOTE:-1}"
RELEASE_CACHE_CONTROL="${RELEASE_CACHE_CONTROL:-public, max-age=31536000, immutable}"
LATEST_CACHE_CONTROL="${LATEST_CACHE_CONTROL:-no-store, max-age=0}"

if [[ -z "$VERSION" ]]; then
  echo "error: VERSION is required" >&2
  exit 1
fi

if [[ -z "$BUCKET" ]]; then
  echo "error: BUCKET is required (or set DEPLOY_ENV plus DEFAULT_BUCKET_STAGING/DEFAULT_BUCKET_PRODUCTION)" >&2
  exit 1
fi

if [[ ! -d "$SOURCE_DIR" ]]; then
  echo "error: SOURCE_DIR not found: $SOURCE_DIR" >&2
  exit 1
fi

checksum_file="$SOURCE_DIR/SHA256SUMS"
if [[ ! -f "$checksum_file" ]]; then
  echo "error: SHA256SUMS not found: $checksum_file" >&2
  exit 1
fi

latest_file="$SOURCE_DIR/latest.txt"
if [[ "$UPDATE_LATEST" == "1" && ! -f "$latest_file" ]]; then
  echo "error: latest.txt not found: $latest_file" >&2
  exit 1
fi

mapfile -t binaries < <(find "$SOURCE_DIR" -maxdepth 1 -type f -name 'nacelle-*' ! -name '*.sha256' ! -name 'SHA256SUMS' ! -name 'latest.txt' | sort)
if [[ "${#binaries[@]}" -eq 0 ]]; then
  echo "error: no nacelle binaries found in $SOURCE_DIR" >&2
  exit 1
fi

for binary in "${binaries[@]}"; do
  binary_name="$(basename "$binary")"
  checksum_sidecar="$SOURCE_DIR/${binary_name}.sha256"

  if ! grep -qE "^[[:xdigit:]]{64}[[:space:]]+\*?${binary_name}$" "$checksum_file"; then
    echo "error: SHA256SUMS missing entry for ${binary_name}" >&2
    exit 1
  fi

  if [[ ! -f "$checksum_sidecar" ]]; then
    echo "error: checksum sidecar not found: $checksum_sidecar" >&2
    exit 1
  fi

  put_object "$PREFIX/$VERSION/$binary_name" "$binary" "$RELEASE_CACHE_CONTROL"
  put_object "$PREFIX/$VERSION/${binary_name}.sha256" "$checksum_sidecar" "$RELEASE_CACHE_CONTROL"
done

put_object "$PREFIX/$VERSION/SHA256SUMS" "$checksum_file" "$RELEASE_CACHE_CONTROL"
if [[ "$UPDATE_LATEST" == "1" ]]; then
  put_object "$PREFIX/latest.txt" "$latest_file" "$LATEST_CACHE_CONTROL"
fi

echo "==> upload completed"
echo "    bucket : $BUCKET"
echo "    env    : ${DEPLOY_ENV:-<manual>}"
echo "    version: $VERSION"
echo "    prefix : $PREFIX"
echo "    latest : $UPDATE_LATEST"
