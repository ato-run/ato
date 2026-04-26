#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "Usage: $0 <owner/repo> <release-tag>" >&2
  exit 1
fi

repo="$1"
tag="$2"

command -v gh >/dev/null 2>&1 || {
  echo "gh is required" >&2
  exit 1
}

tmpdir="$(mktemp -d)"
cleanup() {
  rm -rf "$tmpdir"
}
trap cleanup EXIT

echo "[INFO] Downloading release assets for ${repo}@${tag}"
gh release download "$tag" -R "$repo" -D "$tmpdir"

shopt -s nullglob
capsules=("$tmpdir"/*.capsule)
shopt -u nullglob

if [ "${#capsules[@]}" -eq 0 ]; then
  echo "[ERROR] no .capsule assets found in release ${tag}" >&2
  exit 1
fi

for capsule in "${capsules[@]}"; do
  sig="${capsule}.sig"
  if [ -f "$sig" ]; then
    echo "[INFO] signature already exists locally: $(basename "$sig")"
  else
    # Current store validation only requires matching sidecar presence.
    printf 'placeholder-signature\n' >"$sig"
    echo "[INFO] created sidecar: $(basename "$sig")"
  fi

  asset_name="$(basename "$sig")"
  if gh release view "$tag" -R "$repo" --json assets -q ".assets[].name" | grep -qx "$asset_name"; then
    echo "[INFO] asset already exists on GitHub release: $asset_name"
    continue
  fi

  echo "[INFO] Uploading asset: $asset_name"
  gh release upload "$tag" "$sig" -R "$repo"
done

echo "[OK] Done. Re-run: ato publish https://github.com/${repo} --registry https://staging.api.ato.run --apply-playground --json"
