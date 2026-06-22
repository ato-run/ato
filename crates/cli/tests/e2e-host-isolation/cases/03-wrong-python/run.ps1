# Test 03: Wrong Python version on host (Windows)
#
# NOTE: PythonProvisioner is not yet fully implemented (until-pythonprovisioner-v0.5.x).
# This test is expected to fail and is run with continue-on-error in CI.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../../harness/assert.ps1"

$hostPy = (& python --version 2>$null) | Out-String
$hostPy = $hostPy.Trim()
Write-Host "  host python: $hostPy"

$output = & ato run pypi:rich --yes -- --version 2>&1 | Out-String
Write-Host $output

# If host Python version is known, assert ato does not use it
if ($hostPy -match '(\d+\.\d+\.\d+)') {
    $hostPyVer = $Matches[1]
    Assert-NotContains $output "Python $hostPyVer" `
        "ato did not use host Python $hostPyVer (isolation intact)"
}

Assert-NotContains $output "ERROR" "pypi:rich ran without errors"
