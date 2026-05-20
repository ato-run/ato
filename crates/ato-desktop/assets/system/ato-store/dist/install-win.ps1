# Legacy Windows installer URL.
#
# Prefer:
#   irm https://ato.run/install.ps1 | iex

$ErrorActionPreference = "Stop"
$scriptUrl = if ([string]::IsNullOrWhiteSpace($env:ATO_INSTALL_PS1_URL)) {
  "https://ato.run/install.ps1"
} else {
  $env:ATO_INSTALL_PS1_URL
}

$script = Invoke-RestMethod -Uri $scriptUrl
& ([ScriptBlock]::Create($script)) @args
