#!/usr/bin/env bash
# Download the upstream aether core (macOS Intel) into app resources for bundling.
# Upstream project: CluvexStudio/Aether (prebuilt release binaries).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VER="${1:-2.1.0}"
OUT_DIR="$ROOT/src-tauri/resources/bin/darwin-amd64"
ASSET="aether-macos-x86_64.tar.gz"
URL="https://github.com/CluvexStudio/Aether/releases/download/v${VER}/${ASSET}"

mkdir -p "$OUT_DIR"

# Skip when the exact pinned version is already staged (keeps CI cache hits
# and repeat local builds download-free; a version bump refreshes it).
if [[ -f "$OUT_DIR/aether" && "$(cat "$OUT_DIR/aether-version.txt" 2>/dev/null)" == "v${VER}" ]]; then
  echo "aether v${VER} already staged, skipping download."
  exit 0
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "Downloading $URL …"
curl -fL --retry 3 -o "$TMP/$ASSET" "$URL"

# Integrity: verify against the upstream-published sha256.
curl -fL --retry 3 -o "$TMP/$ASSET.sha256" "$URL.sha256"
(cd "$TMP" && cp "$ASSET" "$(awk '{print $2}' "$ASSET.sha256")" && sha256sum -c "$ASSET.sha256") 

tar -xzf "$TMP/$ASSET" -C "$TMP"
BIN="$(find "$TMP" -maxdepth 2 -type f -name aether | head -1)"
if [[ -z "$BIN" ]]; then
  echo "aether binary not found in archive" >&2
  exit 1
fi

# Only the main binary is bundled; the archive's pt/ helpers (lyrebird,
# psiphon-tunnel-core) back transports this app does not expose.
cp "$BIN" "$OUT_DIR/aether"
chmod +x "$OUT_DIR/aether"
echo "v${VER}" > "$OUT_DIR/aether-version.txt"

echo "Installed:"
ls -lh "$OUT_DIR/aether" "$OUT_DIR/aether-version.txt"
