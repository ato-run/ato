# ─────────────────────────────────────────────────────────────────────────────
# Shared PowerShell assertion library for e2e-host-isolation test cases.
# Dot-source this file at the top of each run.ps1:
#   . "$PSScriptRoot/../../harness/assert.ps1"
#
# All functions throw on failure, making them safe with $ErrorActionPreference = 'Stop'.
# ─────────────────────────────────────────────────────────────────────────────

function Assert-Equal {
    param(
        [string]$Actual,
        [string]$Expected,
        [string]$Message = 'Assert-Equal'
    )
    if ($Actual -ne $Expected) {
        Write-Host "❌ FAIL: $Message"
        Write-Host "  expected: $Expected"
        Write-Host "  actual:   $Actual"
        throw $Message
    }
    Write-Host "✅ PASS: $Message"
}

function Assert-NotEqual {
    param(
        [string]$Actual,
        [string]$Forbidden,
        [string]$Message = 'Assert-NotEqual'
    )
    if ($Actual -eq $Forbidden) {
        Write-Host "❌ FAIL: $Message"
        Write-Host "  got forbidden value: $Forbidden"
        throw $Message
    }
    Write-Host "✅ PASS: $Message"
}

function Assert-Contains {
    param(
        [string]$Haystack,
        [string]$Needle,
        [string]$Message = 'Assert-Contains'
    )
    if ($Haystack -notlike "*$Needle*") {
        Write-Host "❌ FAIL: $Message"
        Write-Host "  expected to contain: $Needle"
        Write-Host "  actual: $Haystack"
        throw $Message
    }
    Write-Host "✅ PASS: $Message"
}

function Assert-NotContains {
    param(
        [string]$Haystack,
        [string]$Needle,
        [string]$Message = 'Assert-NotContains'
    )
    if ($Haystack -like "*$Needle*") {
        Write-Host "❌ FAIL: $Message"
        Write-Host "  must not contain: $Needle"
        Write-Host "  actual: $Haystack"
        throw $Message
    }
    Write-Host "✅ PASS: $Message"
}

function Assert-FileExists {
    param(
        [string]$Path,
        [string]$Message = 'File must exist'
    )
    if (-not (Test-Path -Path $Path)) {
        Write-Host "❌ FAIL: $Message"
        Write-Host "  path: $Path"
        throw "$Message ($Path)"
    }
    Write-Host "✅ PASS: $Message ($Path)"
}

function Assert-DirEmpty {
    param(
        [string]$Dir,
        [string]$Message = 'Directory must be empty'
    )
    $entries = Get-ChildItem -Force -Path $Dir -ErrorAction SilentlyContinue
    if ($entries.Count -gt 0) {
        Write-Host "❌ FAIL: $Message"
        Write-Host "  directory: $Dir"
        Write-Host "  found: $($entries.Name -join ', ')"
        throw $Message
    }
    Write-Host "✅ PASS: $Message ($Dir)"
}
