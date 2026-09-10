#!/usr/bin/env bash
# Download official mihomo (macOS Apple Silicon) into app resources for
# bundling. Stages mihomo + mihomo-version.txt + mihomo-geodata/
# (Country.mmdb + GeoSite.dat — exact casing, macOS is case-sensitive).
# Usage: scripts/fetch-bundled-mihomo-darwin-arm64.sh [version]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VER="${1:-1.19.30}"
PLAT="darwin-arm64"
OUT_DIR="$ROOT/src-tauri/resources/bin/darwin-arm64"
ASSET="mihomo-${PLAT}-v${VER}.gz"
URL="https://github.com/MetaCubeX/mihomo/releases/download/v${VER}/${ASSET}"

mkdir -p "$OUT_DIR/mihomo-geodata"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

if [[ ! -f "$OUT_DIR/mihomo" ]]; then
  echo "Downloading $URL …"
  curl -fL --retry 3 -o "$TMP/$ASSET" "$URL"
  # mihomo darwin assets are a bare gzipped binary.
  gunzip -c "$TMP/$ASSET" > "$OUT_DIR/mihomo"
  chmod +x "$OUT_DIR/mihomo"
  echo "v${VER}" > "$OUT_DIR/mihomo-version.txt"
else
  echo "mihomo already present, skipping download."
fi

# mihomo geodata: Country.mmdb + GeoSite.dat (exact casing).
for pair in "Country.mmdb country.mmdb" "GeoSite.dat geosite.dat"; do
  set -- $pair
  local_name="$1"; remote_name="$2"
  if [[ -f "$OUT_DIR/mihomo-geodata/$local_name" ]]; then continue; fi
  url="https://github.com/MetaCubeX/meta-rules-dat/releases/latest/download/${remote_name}"
  echo "Downloading $url …"
  curl -fL --retry 3 -o "$OUT_DIR/mihomo-geodata/$local_name" "$url"
done

echo "Installed:"
ls -lh "$OUT_DIR"/mihomo "$OUT_DIR"/mihomo-version.txt "$OUT_DIR"/mihomo-geodata/* 2>/dev/null || true
# Self-check may fail on cross-arch/incompatible-microarch runners; don't fail the build.
"$OUT_DIR/mihomo" -v 2>&1 | head -2 || true
