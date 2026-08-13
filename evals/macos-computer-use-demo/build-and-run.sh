#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(mktemp -d "${TMPDIR:-/tmp}/grokptah-computer-use-demo.XXXXXX")
APP="$ROOT/GrokPtah Computer Use Demo.app"
CONTENTS="$APP/Contents"
MACOS="$CONTENTS/MacOS"

mkdir -p "$MACOS"
swiftc "$SCRIPT_DIR/DemoApp.swift" -framework AppKit -o "$MACOS/GrokPtahComputerUseDemo"

plutil -create xml1 "$CONTENTS/Info.plist"
plutil -insert CFBundleExecutable -string GrokPtahComputerUseDemo "$CONTENTS/Info.plist"
plutil -insert CFBundleIdentifier -string com.chriscase.grokptah.computer-use-demo "$CONTENTS/Info.plist"
plutil -insert CFBundleName -string "GrokPtah Computer Use Demo" "$CONTENTS/Info.plist"
plutil -insert CFBundlePackageType -string APPL "$CONTENTS/Info.plist"
plutil -insert CFBundleShortVersionString -string 1.0 "$CONTENTS/Info.plist"
codesign --force --sign - "$APP"
if [ "${1:-}" != "--build-only" ]; then
  open -n "$APP"
fi

printf '%s\n' "$APP"
