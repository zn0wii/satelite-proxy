# Fetch and stage the upstream aether core for Windows (amd64).
# Upstream project: CluvexStudio/Aether (prebuilt release binaries).
# Usage: pwsh scripts/fetch-bundled-aether-windows-amd64.ps1 [-Version 2.1.0]
[CmdletBinding()]
param(
  [string]$Version = "2.1.0",
  [string]$Proxy   = $env:HTTPS_PROXY  # e.g. http://127.0.0.1:7890
)

$ErrorActionPreference = "Stop"

$ROOT = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$DEST = Join-Path $ROOT "src-tauri\resources\bin\windows-amd64"
$TMP  = Join-Path $env:TEMP "satelite-aether-$Version"
$Asset = "aether-windows-x86_64.zip"
$Url = "https://github.com/CluvexStudio/Aether/releases/download/v$Version/$Asset"

$webParams = @{ UseBasicParsing = $true }
if ($Proxy) { $webParams.Proxy = $Proxy }

if (-not (Test-Path $DEST)) { New-Item -ItemType Directory -Path $DEST | Out-Null }

$StagedVersion = "$(Get-Content (Join-Path $DEST "aether-version.txt") -Raw -ErrorAction SilentlyContinue)".Trim()
if ((Test-Path (Join-Path $DEST "aether.exe")) -and $StagedVersion -eq "v$Version") {
  Write-Host "aether v$Version already staged, skipping download."
  return
}

Write-Host "Downloading aether v$Version from $Url"
New-Item -ItemType Directory -Path $TMP -Force | Out-Null
$Zip = Join-Path $TMP $Asset
try {
  Invoke-WebRequest -Uri $Url -OutFile $Zip @webParams
} catch {
  # Fall back to curl.exe (ships with Win10+) which honours env proxies
  Write-Host "Invoke-WebRequest failed, retrying with curl.exe..."
  & curl.exe -sSL -x "$Proxy" -o "$Zip" "$Url"
  if ($LASTEXITCODE -ne 0) { throw "curl download failed (exit $LASTEXITCODE)" }
}

# Integrity: verify against the upstream-published sha256.
$ShaFile = Join-Path $TMP "$Asset.sha256"
Invoke-WebRequest -Uri "$Url.sha256" -OutFile $ShaFile @webParams
$Expected = (Get-Content $ShaFile -Raw).Split(" ")[0].Trim().ToLower()
$Actual = (Get-FileHash -Path $Zip -Algorithm SHA256).Hash.ToLower()
if ($Expected -ne $Actual) { throw "sha256 mismatch: expected $Expected, got $Actual" }
Write-Host "sha256 verified: $Actual"

Expand-Archive -Path $Zip -DestinationPath $TMP -Force

$Bin = Join-Path $TMP "aether.exe"
if (-not (Test-Path $Bin)) { throw "unexpected archive layout: aether.exe not found" }
# Only the main binary is bundled; the archive's pt\ helpers (lyrebird,
# psiphon-tunnel-core) back transports this app does not expose.
Copy-Item -Force $Bin (Join-Path $DEST "aether.exe")
Set-Content -Path (Join-Path $DEST "aether-version.txt") -Value "v$Version" -NoNewline

Write-Host "Staged aether v$Version -> $DEST"
Get-ChildItem $DEST -Filter "aether*" | Format-Table Name, Length
