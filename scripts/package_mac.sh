#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
cargo build --workspace
bundle=target/Homecloud.app
mkdir -p "$bundle/Contents/MacOS"
notices="$bundle/Contents/Resources/licenses/material-symbols"
mkdir -p "$notices"
cp crates/desktop/assets/material/LICENSE crates/desktop/assets/material/NOTICE "$notices/"
for binary in homecloud-desktop homecloud; do
  cp "target/debug/$binary" "$bundle/Contents/MacOS/$binary.new"
  mv -f "$bundle/Contents/MacOS/$binary.new" "$bundle/Contents/MacOS/$binary"
done
cat > "$bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>Homecloud</string>
<key>CFBundleIdentifier</key><string>dev.homecloud.desktop</string>
<key>CFBundleExecutable</key><string>homecloud-desktop</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSAppleEventsUsageDescription</key><string>Homecloud opens an SSH session in Terminal when you choose Open Terminal.</string>
</dict></plist>
PLIST
codesign --force --sign - "$bundle"
printf '%s\n' "Built $bundle"
