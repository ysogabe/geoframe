#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BUILD_CONFIGURATION="${CONFIGURATION:-release}"
APP_DIR="$SCRIPT_DIR/dist/GeoFrame.app"
CONTENTS_DIR="$APP_DIR/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"

swift build --package-path "$SCRIPT_DIR" -c "$BUILD_CONFIGURATION"
BIN_DIR="$(swift build --package-path "$SCRIPT_DIR" -c "$BUILD_CONFIGURATION" --show-bin-path)"

mkdir -p "$MACOS_DIR"
cp "$BIN_DIR/GeoFrameMac" "$MACOS_DIR/GeoFrameMac"
cp "$SCRIPT_DIR/Resources/Info.plist" "$CONTENTS_DIR/Info.plist"

codesign --force --deep --sign - "$APP_DIR"

echo "$APP_DIR"
