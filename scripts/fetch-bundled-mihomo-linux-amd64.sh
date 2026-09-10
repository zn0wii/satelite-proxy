#!/usr/bin/env bash
# Download official mihomo (Linux x86_64) into app resources for
# bundling. Stages mihomo + mihomo-version.txt + mihomo-geodata/
# (Country.mmdb + GeoSite.dat — exact casing, matches the other platforms).
# Usage: scripts/fetch-bundled-mihomo-linux-amd64.sh [version]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VER="${1:-1.19.30}"
OUT_DIR="$ROOT/src-tauri/resources/bin/linux-amd64"
ASSET="mihomo-linux-amd64-v${VER}.gz"
URL="https://github.com/MetaCubeX/mihomo/releases/download/v${VER}/${ASSET}"

mkdir -p "$OUT_DIR/mihomo-geodata"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

if [[ ! -f "$OUT_DIR/mihomo" ]]; then
  echo "Downloading $URL …"
  curl -fL --retry 3 -o "$TMP/$ASSET" "$URL"
  # mihomo linux assets are a bare gzipped binary.
  gunzip -c "$TMP/$ASSET" > "$OUT_DIR/mihomo"
  chmod +x "$OUT_DIR/mihomo"
  echo "v${VER}" > "$OUT_DIR/mihomo-version.txt"
else
  echo "mihomo already present, skipping download."
fi

# mihomo geodata: Country.mmdb + GeoSite.dat, staged from the repo-committed
# snapshot (see resources/geodata/mihomo/ and scripts/fetch-bundled-mihomo-geodata.sh) —
# upstream meta-rules-dat only ships a rolling "latest" release, so a live
# fetch here would defeat the pinned-version guarantee.
SNAPSHOT_DIR="$ROOT/src-tauri/resources/geodata/mihomo"
for pair in "Country.mmdb country.mmdb" "GeoSite.dat geosite.dat"; do
  set -- $pair
  local_name="$1"; snapshot_name="$2"
  if [[ -f "$OUT_DIR/mihomo-geodata/$local_name" ]]; then continue; fi
  if [[ ! -f "$SNAPSHOT_DIR/$snapshot_name" ]]; then
    echo "missing $SNAPSHOT_DIR/$snapshot_name — run scripts/fetch-bundled-mihomo-geodata.sh once and commit the result" >&2
    exit 1
  fi
  cp "$SNAPSHOT_DIR/$snapshot_name" "$OUT_DIR/mihomo-geodata/$local_name"
done

echo "Installed:"
ls -lh "$OUT_DIR"/mihomo "$OUT_DIR"/mihomo-version.txt "$OUT_DIR"/mihomo-geodata/* 2>/dev/null || true
# Self-check may fail on cross-arch/incompatible-microarch runners; don't fail the build.
"$OUT_DIR/mihomo" -v 2>&1 | head -2 || true
