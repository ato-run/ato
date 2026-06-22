# Test 02: Wrong Node version on host (Windows)
#
# Guarantees ato uses its own managed Node even when the host PATH contains a
# different Node version.
#
# Windows limitation: 'source/node' local capsules fail on Windows because ato
# prepends `export PATH=...` (Unix syntax) before `npm install`, which cmd.exe
# does not understand.  Instead, we verify isolation by stripping all host Node
# entries from PATH and confirming ato can still run an npm package (proving it
# uses its own managed runtime rather than the host installation).
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../../harness/assert.ps1"

$hostNode = (& node --version 2>$null) | Out-String
$hostNode = $hostNode.Trim()
if (-not $hostNode) { throw "Pre-condition: host node must be on PATH" }
Write-Host "  host node: $hostNode"

# Strip every nodejs/npm directory from PATH so ato cannot fall back to host Node.
$cleanPath = ($env:Path -split ';' | Where-Object {
    $_ -notmatch '\\nodejs' -and $_ -notmatch '\\npm'
}) -join ';'
$savedPath  = $env:Path
$env:Path   = $cleanPath

try {
    $output = & ato run npm:semver --yes -- 2.0.0 2>&1 | Out-String
    Write-Host $output

    Assert-Contains $output "2.0.0" `
        "ato ran npm:semver with managed Node (host Node stripped from PATH)"
} finally {
    $env:Path = $savedPath
}

