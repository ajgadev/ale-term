#!/bin/sh
# Builds ale-term in release mode and packages it as a macOS app:
#   target/release/ale-term.app
#
# The app is signed "ad hoc" (no Apple account needed), which is enough to
# run it on the Mac that built it.
set -eu

cd "$(dirname "$0")/.."
cargo build --release -p ale-term

VERSION=$(cargo metadata --no-deps --format-version 1 | sed -n 's/.*"name":"ale-term","version":"\([^"]*\)".*/\1/p')
APP=target/release/ale-term.app

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/ale-term "$APP/Contents/MacOS/ale-term"
if [ -f assets/ale-term.icns ]; then
    cp assets/ale-term.icns "$APP/Contents/Resources/ale-term.icns"
fi

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>ale-term</string>
    <key>CFBundleDisplayName</key><string>ale-term</string>
    <key>CFBundleIdentifier</key><string>dev.ale-term.app</string>
    <key>CFBundleExecutable</key><string>ale-term</string>
    <key>CFBundleIconFile</key><string>ale-term</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>${VERSION}</string>
    <key>CFBundleVersion</key><string>${VERSION}</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

codesign --force --sign - "$APP"
echo "Built $APP"
