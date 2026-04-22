# Test 02: Wrong Node version on host (Windows)
#
# Guarantees ato uses managed Node (20.11.0) even when host has a different
# version already on PATH (GitHub Windows runners ship Node 18/20/22).
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../../harness/assert.ps1"

# Pre-condition: host must have Node
$hostNode = (& node --version 2>$null) | Out-String
$hostNode = $hostNode.Trim()
if (-not $hostNode) { throw "Pre-condition: host node must exist on PATH" }
Write-Host "  host node: $hostNode"

$capsuleDir = Join-Path $env:TEMP "ato-test02-$(Get-Random)"
New-Item -ItemType Directory -Force -Path $capsuleDir | Out-Null

try {
    @'
schema_version = "0.3"
name = "test02-wrong-node"
version = "0.1.0"
type = "app"
runtime = "source/node"
runtime_version = "20.11.0"
run = "npm run check"
'@ | Set-Content "$capsuleDir\capsule.toml"

    @'
{
  "name": "test02-wrong-node",
  "version": "0.1.0",
  "private": true,
  "scripts": {
    "check": "node -e \"const v=process.version; console.log('MANAGED_NODE=' + v); if (!v.startsWith('v20.')) { console.error('WRONG_NODE=' + v); process.exit(1); }\""
  }
}
'@ | Set-Content "$capsuleDir\package.json"

    '{"name":"test02-wrong-node","version":"0.1.0","lockfileVersion":3,"requires":true,"packages":{}}' |
        Set-Content "$capsuleDir\package-lock.json"

    $output = & ato run --yes $capsuleDir 2>&1 | Out-String
    Write-Host $output

    Assert-Contains $output "MANAGED_NODE=v20." `
        "ato used managed Node 20.x (host was $hostNode)"

    # Host PATH must not be permanently mutated
    $hostNodeAfter = (& node --version 2>$null) | Out-String
    $hostNodeAfter = $hostNodeAfter.Trim()
    Assert-Equal $hostNodeAfter $hostNode "ato did not mutate host PATH after run"
} finally {
    Remove-Item -Recurse -Force $capsuleDir -ErrorAction SilentlyContinue
}
