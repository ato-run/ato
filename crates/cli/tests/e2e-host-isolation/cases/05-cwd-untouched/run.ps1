# Test 05: User cwd untouched (Windows)
#
# Runs ato in a fresh empty directory and asserts it leaves no artifacts behind.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../../harness/assert.ps1"

$sentinel = Join-Path $env:TEMP "ato-test05-$(Get-Random)"
New-Item -ItemType Directory -Force -Path $sentinel | Out-Null

try {
    Push-Location $sentinel
    try {
        & ato run npm:semver --yes -- 1.0.0 2>&1 | Out-Null
    } finally {
        Pop-Location
    }

    Assert-DirEmpty $sentinel "user cwd was not polluted after ato run npm:semver"
} finally {
    Remove-Item -Recurse -Force $sentinel -ErrorAction SilentlyContinue
}
