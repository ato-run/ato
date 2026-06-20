# Test 08: Windows PATH case-insensitivity
#
# Windows resolves PATH case-insensitively and the env block can carry both
# "Path" and "PATH" keys simultaneously (Node.js child_process bug territory).
# This test injects bogus directories under both keys and asserts ato resolves
# its managed runtime without including either bogus entry.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../../harness/assert.ps1"

$bogusUpper = "C:\ato-bogus-UPPER-$(Get-Random)"
$bogusLower = "C:\ato-bogus-lower-$(Get-Random)"

# Set both casing variants — Windows env is case-insensitive but PowerShell
# can hold them as separate keys in the process env block.
$env:PATH = "$bogusUpper;$env:PATH"
$env:Path = "$bogusLower;$env:Path"

$output = & ato run npm:semver --yes -- 1.0.0 2>&1 | Out-String
Write-Host $output

Assert-NotContains $output "bogus-UPPER" `
    "PATH=bogusUpper did not leak into ato resolution"
Assert-NotContains $output "bogus-lower" `
    "Path=bogusLower did not leak into ato resolution"

Assert-Contains $output "1.0.0" "npm:semver produced expected output"
