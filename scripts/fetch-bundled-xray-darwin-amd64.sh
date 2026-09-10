#!/usr/bin/env bash
# Download official Xray-core (macOS Intel) into app resources for bundling.
# Stages xray + geosite.dat + geoip.dat + xray-version.txt.
# Usage: scripts/fetch-bundled-xray-darwin-amd64.sh [version]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VER="${1:-26.3.27}"
OUT_DIR="$ROOT/src-tauri/resources/bin/darwin-amd64"
ASSET="Xray-macos-64.zip"
URL="https://github.com/XTLS/Xray-core/releases/download/v${VER}/${ASSET}"

mkdir -p "$OUT_DIR"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "Downloading $URL …"
curl -fL --retry 3 -o "$TMP/$ASSET" "$URL"
unzip -q "$TMP/$ASSET" -d "$TMP/xray"
BIN="$(find "$TMP/xray" -type f -name xray | head -1)"
if [[ -z "$BIN" ]]; then
  echo "xray binary not found in archive" >&2
  exit 1
fi

cp "$BIN" "$OUT_DIR/xray"
chmod +x "$OUT_DIR/xray"
# geodata ships alongside the binary: stage it so geosite:/geoip: routing works
for dat in geosite.dat geoip.dat; do
  SRC="$(find "$TMP/xray" -type f -name "$dat" | head -1)"
  if [[ -n "$SRC" ]]; then
    cp "$SRC" "$OUT_DIR/$dat"
  else
    echo "warning: $dat missing from archive; geo rules will need a runtime download" >&2
  fi
done
echo "v${VER}" > "$OUT_DIR/xray-version.txt"

echo "Installed:"
ls -lh "$OUT_DIR/xray" "$OUT_DIR/xray-version.txt" "$OUT_DIR"/geosite.dat "$OUT_DIR"/geoip.dat 2>/dev/null || true
"$OUT_DIR/xray" -version | head -2
