# Test 06: Child process spawned by npm inherits managed PATH (Windows)
#
# Verifies that child processes spawned by npm scripts see managed Node.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../../harness/assert.ps1"

$capsuleDir = Join-Path $env:TEMP "ato-test06-$(Get-Random)"
New-Item -ItemType Directory -Force -Path $capsuleDir | Out-Null

try {
    @'
schema_version = "0.3"
name = "test06-child-spawn"
version = "0.1.0"
type = "app"
runtime = "source/node"
runtime_version = "20.11.0"
run = "npm run check-child"
'@ | Set-Content "$capsuleDir\capsule.toml"

    @'
{
  "name": "test06-child-spawn",
  "version": "0.1.0",
  "private": true,
  "scripts": {
    "check-child": "node -e \"const v=process.version; console.log('CHILD_NODE=' + v); if (!v.startsWith('v20.')) { console.error('WRONG_CHILD_NODE=' + v); process.exit(1); }\""
  }
}
'@ | Set-Content "$capsuleDir\package.json"

    '{"name":"test06-child-spawn","version":"0.1.0","lockfileVersion":3,"requires":true,"packages":{}}' |
        Set-Content "$capsuleDir\package-lock.json"

    $output = & ato run --yes $capsuleDir 2>&1 | Out-String
    Write-Host $output

    Assert-Contains $output "CHILD_NODE=v20." `
        "child process spawned by npm saw managed Node 20.x"
} finally {
    Remove-Item -Recurse -Force $capsuleDir -ErrorAction SilentlyContinue
}
