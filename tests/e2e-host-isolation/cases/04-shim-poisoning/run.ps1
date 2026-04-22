# Test 04: Shim poisoning (Windows)
#
# Places a fake node.exe in %LOCALAPPDATA%\poison and prepends it to $env:Path,
# then asserts ato never calls it.
# Uses LOCALAPPDATA (user-writable) to avoid requiring elevation.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../../harness/assert.ps1"

$poisonDir = Join-Path $env:LOCALAPPDATA "ato-shim-poison-$(Get-Random)"
New-Item -ItemType Directory -Force -Path $poisonDir | Out-Null

$poisonMarker = "SHIM-POISONED-NODE-$PID"

# Batch file echoes the poison marker when called as "node"
@"
@echo $poisonMarker
"@ | Set-Content "$poisonDir\node.bat"

# Prepend poison dir to the current session's Path
$env:Path = "$poisonDir;$env:Path"

try {
    $output = & ato run npm:semver --yes -- 2.0.0 2>&1 | Out-String
    Write-Host $output

    Assert-NotContains $output $poisonMarker `
        "ato did not invoke poisoned node from $poisonDir"

    Assert-Contains $output "2.0.0" "npm:semver produced expected output"
} finally {
    Remove-Item -Recurse -Force $poisonDir -ErrorAction SilentlyContinue
}
