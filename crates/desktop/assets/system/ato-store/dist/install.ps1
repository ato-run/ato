# Ato installer for Windows (PowerShell).
#
# Primary Windows path for the unsigned v0.5 phase:
#   irm https://ato.run/install.ps1 | iex
#
# The installer downloads the ZIP release asset, removes Mark-of-the-Web with
# Unblock-File, expands into a user-scoped directory, and updates the user PATH.
# The MSI path is intentionally disabled by default until Windows code signing
# is in place.

[CmdletBinding()]
param(
  [string]$Version = $env:ATO_RELEASE_VERSION,
  [string]$InstallDir = $env:ATO_INSTALL_DIR,
  [switch]$NoModifyPath,
  [switch]$WithDesktop
)

$ErrorActionPreference = "Stop"

$releaseRepo = if ([string]::IsNullOrWhiteSpace($env:ATO_RELEASE_REPO)) {
  "ato-run/ato"
} else {
  $env:ATO_RELEASE_REPO.Trim()
}

$releaseVersion = if ([string]::IsNullOrWhiteSpace($Version)) {
  "latest"
} else {
  $Version.Trim()
}

$githubApiBaseUrl = if ([string]::IsNullOrWhiteSpace($env:ATO_GITHUB_API_BASE_URL)) {
  "https://api.github.com"
} else {
  $env:ATO_GITHUB_API_BASE_URL.TrimEnd("/")
}

$installDir = if ([string]::IsNullOrWhiteSpace($InstallDir)) {
  Join-Path $env:LOCALAPPDATA "Programs\ato\bin"
} else {
  $InstallDir
}

$headers = @{
  Accept = "application/vnd.github+json"
  "X-GitHub-Api-Version" = "2022-11-28"
}

function Get-NormalizedReleaseTag {
  param([string]$InputVersion)

  if ($InputVersion -eq "latest") { return "latest" }
  if ($InputVersion.StartsWith("v")) { return $InputVersion }
  return "v$InputVersion"
}

function Invoke-Download {
  param(
    [string]$Uri,
    [string]$OutFile
  )

  Invoke-WebRequest -Uri $Uri -OutFile $OutFile -UseBasicParsing
  if (Get-Command Unblock-File -ErrorAction SilentlyContinue) {
    Unblock-File -Path $OutFile
  }
}

function Add-UserPath {
  param([string]$PathToAdd)

  $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
  if ([string]::IsNullOrWhiteSpace($userPath)) { $userPath = "" }

  $entries = $userPath -split ";" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
  $alreadyPresent = $entries | Where-Object { $_.TrimEnd("\") -ieq $PathToAdd.TrimEnd("\") } | Select-Object -First 1

  if (-not $alreadyPresent) {
    $updatedPath = if ($userPath) { "$userPath;$PathToAdd" } else { $PathToAdd }
    [Environment]::SetEnvironmentVariable("Path", $updatedPath, "User")
    Write-Host "PATH added: $PathToAdd"
  }

  $processEntries = $env:Path -split ";" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
  $processHasPath = $processEntries | Where-Object { $_.TrimEnd("\") -ieq $PathToAdd.TrimEnd("\") } | Select-Object -First 1
  if (-not $processHasPath) {
    $env:Path = "$PathToAdd;$env:Path"
  }
}

function Install-CliArchive {
  $releaseTag = Get-NormalizedReleaseTag $releaseVersion
  $releaseApiUrl = if ($releaseTag -eq "latest") {
    "$githubApiBaseUrl/repos/$releaseRepo/releases/latest"
  } else {
    "$githubApiBaseUrl/repos/$releaseRepo/releases/tags/$releaseTag"
  }

  Write-Host "Resolving ato release from GitHub..."
  $release = Invoke-RestMethod -Uri $releaseApiUrl -Headers $headers -Method Get
  $resolvedTag = [string]$release.tag_name
  if ([string]::IsNullOrWhiteSpace($resolvedTag)) {
    throw "GitHub release metadata did not contain tag_name."
  }

  $resolvedVersion = $resolvedTag.TrimStart("v")
  $candidateAssetNames = @(
    "ato-cli-$resolvedVersion-x86_64-pc-windows-msvc.zip",
    "ato-cli-x86_64-pc-windows-msvc.zip"
  )

  $asset = $release.assets | Where-Object { $candidateAssetNames -contains $_.name } | Select-Object -First 1
  if (-not $asset) {
    $assetNames = ($release.assets | ForEach-Object { $_.name }) -join ", "
    throw "Windows ZIP asset not found in GitHub release $resolvedTag. Looked for: $($candidateAssetNames -join ', '). Assets: $assetNames"
  }

  $artifactUrl = [string]$asset.browser_download_url
  if ([string]::IsNullOrWhiteSpace($artifactUrl)) {
    throw "GitHub release asset did not expose browser_download_url."
  }

  $tmpRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("ato-install-" + [Guid]::NewGuid().ToString("N"))
  $tmpArchive = Join-Path $tmpRoot "ato-cli.zip"
  $extractDir = Join-Path $tmpRoot "extract"

  try {
    New-Item -ItemType Directory -Force -Path $tmpRoot | Out-Null
    New-Item -ItemType Directory -Force -Path $extractDir | Out-Null
    New-Item -ItemType Directory -Force -Path $installDir | Out-Null

    Write-Host "Downloading $($asset.name)..."
    Invoke-Download -Uri $artifactUrl -OutFile $tmpArchive

    Expand-Archive -Path $tmpArchive -DestinationPath $extractDir -Force
    if (Get-Command Unblock-File -ErrorAction SilentlyContinue) {
      Get-ChildItem -Path $extractDir -Recurse -File | Unblock-File
    }

    $sourceExe = Get-ChildItem -Path $extractDir -Filter "ato.exe" -Recurse -File | Select-Object -First 1
    if (-not $sourceExe) {
      throw "ato.exe not found after extraction."
    }

    $targetExe = Join-Path $installDir "ato.exe"
    Copy-Item -Path $sourceExe.FullName -Destination $targetExe -Force
    if (Get-Command Unblock-File -ErrorAction SilentlyContinue) {
      Unblock-File -Path $targetExe
    }

    if (-not $NoModifyPath) {
      Add-UserPath -PathToAdd $installDir
    }

    Write-Host "CLI install OK: $targetExe"
    & $targetExe --version
  } finally {
    Remove-Item $tmpRoot -Recurse -Force -ErrorAction SilentlyContinue
  }
}

if ($WithDesktop) {
  Write-Warning "Windows Desktop MSI install is disabled until signed installers ship. Installing the CLI ZIP only."
}

Install-CliArchive
Write-Host "Install OK"
