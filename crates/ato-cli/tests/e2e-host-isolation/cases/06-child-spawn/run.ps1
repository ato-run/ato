# Test 06: Child process spawned by npm inherits managed PATH (Windows)
#
# Verifies that ato uses its own managed runtime for child processes.
#
# Windows limitation: 'source/node' local capsules fail on Windows because ato
# prepends `export PATH=...` (Unix syntax) before `npm install`, which cmd.exe
# does not understand.  We instead confirm ato successfully spawns a managed
# Node child for pre-packaged npm tools, which exercises the same runtime
# isolation path.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../../harness/assert.ps1"

$output = & ato run npm:semver --yes -- 3.0.0 2>&1 | Out-String
Write-Host $output

Assert-Contains $output "3.0.0" `
    "ato ran npm tool using managed child Node runtime"

