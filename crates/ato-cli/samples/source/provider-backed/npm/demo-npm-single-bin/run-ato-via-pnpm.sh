#!/bin/sh
set -eu

: "${ATO_PROVIDER_NPM_REGISTRY:?set ATO_PROVIDER_NPM_REGISTRY to a test npm registry URL}"

script_dir=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../../../../../" && pwd)
work_root="$repo_root/.ato/samples-scratch/provider-backed-oneliners/demo-npm-single-bin-pnpm"

mkdir -p "$work_root"
cd "$work_root"

printf 'hello from pnpm provider package\n' > input.txt

export NPM_CONFIG_REGISTRY="$ATO_PROVIDER_NPM_REGISTRY"
export npm_config_registry="$ATO_PROVIDER_NPM_REGISTRY"
export NPM_CONFIG_CACHE="$work_root/.npm-cache"
export PNPM_HOME="$work_root/.pnpm-home"
export PNPM_STORE_DIR="$work_root/.pnpm-store"

ato run --yes --via pnpm npm:demo-npm-single-bin -- ./input.txt -o ./output.json