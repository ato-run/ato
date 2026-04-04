#!/bin/sh
set -eu

: "${ATO_PROVIDER_NPM_REGISTRY:?set ATO_PROVIDER_NPM_REGISTRY to a test npm registry URL}"

script_dir=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../../../../../" && pwd)
work_root="$repo_root/.tmp/provider-backed-oneliners/demo-npm-needs-install-script"

mkdir -p "$work_root"
cd "$work_root"

export NPM_CONFIG_REGISTRY="$ATO_PROVIDER_NPM_REGISTRY"
export npm_config_registry="$ATO_PROVIDER_NPM_REGISTRY"
export NPM_CONFIG_CACHE="$work_root/.npm-cache"

ato run --yes npm:demo-npm-needs-install-script