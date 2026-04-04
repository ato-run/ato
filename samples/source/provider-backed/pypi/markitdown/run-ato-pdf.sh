#!/bin/sh
set -eu

: "${ATO_PROVIDER_PYPI_INDEX_URL:?set ATO_PROVIDER_PYPI_INDEX_URL to a test simple index URL}"

script_dir=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../../../../../" && pwd)
work_root="$repo_root/.tmp/provider-backed-oneliners/markitdown-pdf"

mkdir -p "$work_root"
cd "$work_root"

printf '%%PDF-1.4\nfixture\n' > input.pdf

export UV_INDEX_URL="$ATO_PROVIDER_PYPI_INDEX_URL"
export PIP_INDEX_URL="$ATO_PROVIDER_PYPI_INDEX_URL"
if [ -n "${ATO_PROVIDER_PYPI_INSECURE_HOST:-}" ]; then
  export UV_INSECURE_HOST="$ATO_PROVIDER_PYPI_INSECURE_HOST"
fi

ato run --yes 'pypi:markitdown[pdf]' -- ./input.pdf -o ./out.md