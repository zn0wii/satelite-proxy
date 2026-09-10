# Fetch and stage the bundled sing-box core for Windows (amd64).
# Downloads sing-box + libcronet.dll into src-tauri/resources/bin/windows-amd64/.
# Usage: pwsh scripts/fetch-bundled-core-windows-amd64.ps1 [-Version 1.13.18]
[CmdletBinding()]
param(
  [string]$Version = "1.13.18",
  [string]$Proxy   = $env:HTTPS_PROXY  # e.g. http://127.0.0.1:7890
)

$ErrorActionPreference = "Stop"

$ROOT = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$DEST = Join-Path $ROOT "src-tauri\resources\bin\windows-amd64"
$TMP  = Join-Path $env:TEMP "satelite-singbox-$Version"

$Url = "https://github.com/SagerNet/sing-box/releases/download/v$Version/sing-box-$Version-windows-amd64.zip"

# Optional proxy for Invoke-WebRequest
$webParams = @{ UseBasicParsing = $true }
if ($Proxy) { $webParams.Proxy = $Proxy }

if (-not (Test-Path $DEST)) { New-Item -ItemType Directory -Path $DEST | Out-Null }

if (Test-Path (Join-Path $DEST "sing-box.exe")) {
  Write-Host "sing-box.exe already present, skipping download."
  return
}

Write-Host "Downloading sing-box v$Version from $Url"
if ($Proxy) { Write-Host "(via proxy $Proxy)" }
New-Item -ItemType Directory -Path $TMP -Force | Out-Null
$Zip = Join-Path $TMP "sing-box.zip"
try {
  Invoke-WebRequest -Uri $Url -OutFile $Zip @webParams
} catch {
  # Fall back to curl.exe (ships with Win10+) which honours env proxies
  Write-Host "Invoke-WebRequest failed, retrying with curl.exe..."
  & curl.exe -sSL -x "$Proxy" -o "$Zip" "$Url"
  if ($LASTEXITCODE -ne 0) { throw "curl download failed (exit $LASTEXITCODE)" }
}

Write-Host "Extracting..."
Expand-Archive -Path $Zip -DestinationPath $TMP -Force

$Inner = Get-ChildItem -Path $TMP -Directory | Where-Object Name -like "sing-box-*-windows-amd64" | Select-Object -First 1
if (-not $Inner) { throw "unexpected archive layout: inner dir not found" }

Copy-Item -Force (Join-Path $Inner.FullName "sing-box.exe") (Join-Path $DEST "sing-box.exe")
# libcronet.dll is shipped alongside in recent releases; copy if present
$libcronet = Join-Path $Inner.FullName "libcronet.dll"
if (Test-Path $libcronet) { Copy-Item -Force $libcronet (Join-Path $DEST "libcronet.dll") }
Set-Content -Path (Join-Path $DEST "version.txt") -Value "v$Version" -NoNewline

Write-Host "Staged sing-box v$Version -> $DEST"
Get-ChildItem $DEST | Format-Table Name, Length
