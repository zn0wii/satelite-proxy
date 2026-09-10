#!/usr/bin/env bash
# Build the frontend + Tauri app and package it as a macOS .dmg.
# Must be run on macOS (native or the requested arch via Rust target).
#
# Usage:
#   scripts/build-dmg.sh                      # auto-detect host arch, all three cores (default)
#   scripts/build-dmg.sh --arch arm64         # Apple Silicon
#   scripts/build-dmg.sh --arch intel         # Intel (x86_64)
#   scripts/build-dmg.sh --arch intel 1.13.18 # pin bundled sing-box version
#   scripts/build-dmg.sh --singbox-only       # slim build: sing-box only, drop Xray + mihomo
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

usage() {
  sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
}

ARCH="auto"
CORE_VER=""
SINGBOX_ONLY=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    --arch)
      if [[ $# -lt 2 ]]; then
        echo "--arch requires a value (arm64|intel|auto)" >&2
        exit 1
      fi
      ARCH="$2"
      shift 2
      ;;
    --arch=*)
      ARCH="${1#*=}"
      shift
      ;;
    --singbox-only)
      SINGBOX_ONLY=1
      shift
      ;;
    -*)
      echo "unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
    *)
      if [[ -n "$CORE_VER" ]]; then
        echo "unexpected argument: $1" >&2
        exit 1
      fi
      CORE_VER="$1"
      shift
      ;;
  esac
done

host_arch() {
  case "$(uname -m)" in
    arm64|aarch64) echo "arm64" ;;
    x86_64|amd64) echo "intel" ;;
    *) echo "unknown" ;;
  esac
}

normalize_arch() {
  case "$1" in
    auto|"")
      local detected
      detected="$(host_arch)"
      if [[ "$detected" == "unknown" ]]; then
        echo "cannot auto-detect arch from $(uname -m); pass --arch arm64 or --arch intel" >&2
        exit 1
      fi
      echo "$detected"
      ;;
    arm64|aarch64|apple-silicon|silicon) echo "arm64" ;;
    intel|amd64|x86_64|x64) echo "intel" ;;
    *)
      echo "unknown --arch '$1' (use arm64, intel, or auto)" >&2
      exit 1
      ;;
  esac
}

ARCH="$(normalize_arch "$ARCH")"

if [[ "$ARCH" == "intel" ]]; then
  TRIPLE="x86_64-apple-darwin"
  CORE_DIR="darwin-amd64"
  FETCH_SCRIPT="$ROOT/scripts/fetch-bundled-core-darwin-amd64.sh"
  XRAY_FETCH="$ROOT/scripts/fetch-bundled-xray-darwin-amd64.sh"
  MIHOMO_FETCH="$ROOT/scripts/fetch-bundled-mihomo-darwin-amd64.sh"
else
  TRIPLE="aarch64-apple-darwin"
  CORE_DIR="darwin-arm64"
  FETCH_SCRIPT="$ROOT/scripts/fetch-bundled-core-darwin-arm64.sh"
  XRAY_FETCH="$ROOT/scripts/fetch-bundled-xray-darwin-arm64.sh"
  MIHOMO_FETCH="$ROOT/scripts/fetch-bundled-mihomo-darwin-arm64.sh"
fi
case "$ARCH" in
  intel) SINGBOX_ONLY_CONFIG="$ROOT/src-tauri/tauri.singbox-darwin-amd64.conf.json" ;;
  *)     SINGBOX_ONLY_CONFIG="$ROOT/src-tauri/tauri.singbox-darwin-arm64.conf.json" ;;
esac

command -v pnpm >/dev/null || { echo "pnpm not found in PATH" >&2; exit 1; }
command -v cargo >/dev/null || { echo "cargo not found in PATH" >&2; exit 1; }

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "DMG packaging must run on macOS (current: $(uname -s))." >&2
  exit 1
fi

if command -v rustup >/dev/null; then
  if ! rustup target list --installed | grep -q "$TRIPLE"; then
    echo "Installing Rust target $TRIPLE..."
    rustup target add "$TRIPLE"
  fi
fi

CORE_BIN="$ROOT/src-tauri/resources/bin/$CORE_DIR/sing-box"
if [[ ! -x "$CORE_BIN" ]]; then
  echo "sing-box core missing ($CORE_DIR), fetching..."
  if [[ -n "$CORE_VER" ]]; then
    "$FETCH_SCRIPT" "$CORE_VER"
  else
    "$FETCH_SCRIPT"
  fi
fi

RULE_SETS_DIR="$ROOT/src-tauri/resources/rule-sets"
if [[ ! -f "$RULE_SETS_DIR/system-geosite-cn.srs" ]]; then
  echo "built-in rule sets missing, fetching..."
  "$ROOT/scripts/fetch-bundled-rule-sets.sh"
fi

# --- extra cores: bundle Xray + mihomo (default), or switch to the
# sing-box-only config. The base config lists their resources too — a
# missing file fails the bundler, so the sing-box-only build swaps in the
# slim resource overlay.
CONFIG_FILE=""
if [[ "$ARCH" == "intel" && "$SINGBOX_ONLY" == "0" ]]; then
  CONFIG_FILE="$ROOT/src-tauri/tauri.macos-intel.conf.json"
fi
if [[ "$SINGBOX_ONLY" == "0" ]]; then
  echo "Bundling Xray + mihomo alongside sing-box (pass --singbox-only to drop them)."
  if [[ ! -x "$ROOT/src-tauri/resources/bin/$CORE_DIR/xray" ]]; then
    echo "xray core missing ($CORE_DIR), fetching..."
    "$XRAY_FETCH"
  fi
  if [[ ! -x "$ROOT/src-tauri/resources/bin/$CORE_DIR/mihomo" ]]; then
    echo "mihomo core missing ($CORE_DIR), fetching..."
    "$MIHOMO_FETCH"
  fi
else
  echo "SingboxOnly: bundling sing-box only."
  CONFIG_FILE="$SINGBOX_ONLY_CONFIG"
fi

echo "Installing JS dependencies..."
pnpm install --frozen-lockfile

echo "Building $ARCH ($TRIPLE) and packaging dmg..."
BUILD_ARGS=(--target "$TRIPLE" --bundles dmg)
if [[ -n "$CONFIG_FILE" ]]; then
  BUILD_ARGS+=(--config "$CONFIG_FILE")
fi
pnpm tauri build "${BUILD_ARGS[@]}"

DMG=""
for DMG_DIR in \
  "$ROOT/src-tauri/target/${TRIPLE}/release/bundle/dmg" \
  "$ROOT/src-tauri/target/release/bundle/dmg"
do
  if [[ -d "$DMG_DIR" ]]; then
    DMG="$(find "$DMG_DIR" -name '*.dmg' -maxdepth 1 | head -1 || true)"
    if [[ -n "$DMG" ]]; then
      break
    fi
  fi
done

if [[ -z "$DMG" ]]; then
  echo "Build finished but no .dmg found under src-tauri/target/**/release/bundle/dmg" >&2
  exit 1
fi

echo "DMG ready ($ARCH): $DMG"
