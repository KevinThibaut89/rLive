#!/usr/bin/env bash
# Build rLive in release mode and wrap it in a macOS .app bundle (target/rLive.app).
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release -p rlive-app
APP=target/rLive.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/rlive "$APP/Contents/MacOS/rLive"
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>rLive</string>
  <key>CFBundleDisplayName</key><string>rLive</string>
  <key>CFBundleIdentifier</key><string>com.github.kevinthibaut89.rlive</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleExecutable</key><string>rLive</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>rLive records and monitors audio from the input you choose in Settings.</string>
</dict>
</plist>
PLIST
# Ad-hoc sign so macOS lets the app load plugins and access audio devices.
codesign --force --deep --sign - "$APP" 2>/dev/null || true
echo "Built $APP"
