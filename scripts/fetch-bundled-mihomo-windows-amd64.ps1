# Fetch and stage the bundled mihomo core for Windows (amd64).
# Downloads mihomo.exe from the mihomo-windows-amd64-v{ver}.zip release
# asset; geodata (Country.mmdb + GeoSite.dat) is copied from the
# repo-committed snapshot in resources/geodata/mihomo/ (see
# scripts/fetch-bundled-mihomo-geodata.sh) into
# src-tauri/resources/bin/windows-amd64/mihomo-geodata/.
# wintun.dll (Windows tun) is shared with the Xray staging — this script
# only fetches it when missing.
# Usage: pwsh scripts/fetch-bundled-mihomo-windows-amd64.ps1 [-Version 1.19.30] [-Proxy http://127.0.0.1:7890]
[CmdletBinding()]
param(
  [string]$Version = "1.19.30",
  [string]$Proxy   = $env:HTTPS_PROXY  # e.g. http://127.0.0.1:7890
)

$ErrorActionPreference = "Stop"

$ROOT = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$DEST = Join-Path $ROOT "src-tauri\resources\bin\windows-amd64"
$GEO  = Join-Path $DEST "mihomo-geodata"
$TMP  = Join-Path $env:TEMP "satelite-mihomo-$Version"

$Url = "https://github.com/MetaCubeX/mihomo/releases/download/v$Version/mihomo-windows-amd64-v$Version.zip"

$webParams = @{ UseBasicParsing = $true }
if ($Proxy) { $webParams.Proxy = $Proxy }

if (-not (Test-Path $DEST)) { New-Item -ItemType Directory -Path $DEST | Out-Null }
if (-not (Test-Path $GEO))  { New-Item -ItemType Directory -Path $GEO  | Out-Null }

if (Test-Path (Join-Path $DEST "mihomo.exe")) {
  Write-Host "mihomo.exe already present, skipping download."
} else {
  Write-Host "Downloading mihomo v$Version from $Url"
  if ($Proxy) { Write-Host "(via proxy $Proxy)" }
  New-Item -ItemType Directory -Path $TMP -Force | Out-Null
  $Zip = Join-Path $TMP "mihomo.zip"
  try {
    Invoke-WebRequest -Uri $Url -OutFile $Zip @webParams
  } catch {
    Write-Host "Invoke-WebRequest failed, retrying with curl.exe..."
    & curl.exe -sSL -x "$Proxy" -o "$Zip" "$Url"
    if ($LASTEXITCODE -ne 0) { throw "curl download failed (exit $LASTEXITCODE)" }
  }

  Write-Host "Extracting..."
  Expand-Archive -Path $Zip -DestinationPath $TMP -Force
  # mihomo zips carry a versioned inner exe (mihomo-windows-amd64.exe).
  $Exe = Get-ChildItem -Path $TMP -Recurse -Filter "mihomo*.exe" | Select-Object -First 1
  if (-not $Exe) { throw "mihomo.exe not found in archive" }
  Copy-Item -Force $Exe.FullName (Join-Path $DEST "mihomo.exe")

  Set-Content -Path (Join-Path $DEST "mihomo-version.txt") -Value "v$Version" -NoNewline
}

# wintun.dll powers the tun inbound (NOT shipped in the mihomo zip);
# shared file name with the Xray staging.
if (-not (Test-Path (Join-Path $DEST "wintun.dll"))) {
  $WintunZip = Join-Path $TMP "wintun.zip"
  $WintunUrl = "https://www.wintun.net/builds/wintun-0.14.1.zip"
  Write-Host "Downloading wintun.dll from $WintunUrl"
  try {
    Invoke-WebRequest -Uri $WintunUrl -OutFile $WintunZip @webParams
  } catch {
    & curl.exe -sSL -x "$Proxy" -o "$WintunZip" "$WintunUrl"
    if ($LASTEXITCODE -ne 0) { throw "curl wintun download failed (exit $LASTEXITCODE)" }
  }
  Expand-Archive -Path $WintunZip -DestinationPath (Join-Path $TMP "wintun") -Force
  Copy-Item -Force (Join-Path $TMP "wintun\wintun\bin\amd64\wintun.dll") (Join-Path $DEST "wintun.dll")
}

# mihomo geodata: Country.mmdb + GeoSite.dat, staged from the repo-committed
# snapshot (see resources/geodata/mihomo/ and
# scripts/fetch-bundled-mihomo-geodata.sh) — upstream meta-rules-dat only
# ships a rolling "latest" release, so a live fetch here would defeat the
# pinned-version guarantee.
$SnapshotDir = Join-Path $ROOT "src-tauri\resources\geodata\mihomo"
foreach ($file in @(@{ Name = "Country.mmdb"; Snapshot = "country.mmdb" },
                    @{ Name = "GeoSite.dat"; Snapshot = "geosite.dat" })) {
  $target = Join-Path $GEO $file.Name
  if (Test-Path $target) { continue }
  $source = Join-Path $SnapshotDir $file.Snapshot
  if (-not (Test-Path $source)) {
    throw "missing $source - run scripts/fetch-bundled-mihomo-geodata.sh once and commit the result"
  }
  Copy-Item -Force $source $target
}

Write-Host "Staged mihomo v$Version -> $DEST"
Get-ChildItem $DEST -Filter "mihomo*" | Format-Table Name, Length
Get-ChildItem $GEO | Format-Table Name, Length
