#!/bin/sh
set -eu

: "${ATO_PROVIDER_PYPI_INDEX_URL:?set ATO_PROVIDER_PYPI_INDEX_URL to a test simple index URL}"

script_dir=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../../../../../" && pwd)
work_root="$repo_root/.ato/samples-scratch/provider-backed-oneliners/demo-provider-pdf"

mkdir -p "$work_root"
cd "$work_root"

printf 'hello from provider package\n' > input.txt

export UV_INDEX_URL="$ATO_PROVIDER_PYPI_INDEX_URL"
export PIP_INDEX_URL="$ATO_PROVIDER_PYPI_INDEX_URL"
if [ -n "${ATO_PROVIDER_PYPI_INSECURE_HOST:-}" ]; then
  export UV_INSECURE_HOST="$ATO_PROVIDER_PYPI_INSECURE_HOST"
fi

ato run --yes 'pypi:demo-provider[pdf]' -- ./input.txt -o ./output.json