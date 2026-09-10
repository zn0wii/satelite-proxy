#!/usr/bin/env bash
# One-off snapshot fetch for mihomo geodata (Country.mmdb + GeoSite.dat).
# Upstream MetaCubeX/meta-rules-dat only publishes a rolling "latest"
# release (no versioned tags), so there is nothing to pin a URL to. Instead
# we snapshot the files once here and commit them under
# src-tauri/resources/geodata/mihomo/ — every per-platform
# fetch-bundled-mihomo-*.sh script copies from that committed snapshot
# instead of hitting the network, so the bundled geodata is reproducible
# across CI runs.
#
# Run this manually whenever you want to refresh the pinned snapshot, then
# `git add src-tauri/resources/geodata/mihomo/*` and commit.
# Usage: scripts/fetch-bundled-mihomo-geodata.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT_DIR="$ROOT/src-tauri/resources/geodata/mihomo"
mkdir -p "$OUT_DIR"

for name in country.mmdb geosite.dat; do
  url="https://github.com/MetaCubeX/meta-rules-dat/releases/latest/download/${name}"
  echo "Downloading $url …"
  curl -fL --retry 3 -o "$OUT_DIR/$name" "$url"
done

echo "Snapshot updated:"
ls -lh "$OUT_DIR"
echo
echo "Next: git add src-tauri/resources/geodata/mihomo/* && git commit"
