#!/bin/sh
# Builds ale-term in release mode and packages it as a macOS app:
#   target/release/ale-term.app
#
# The app is signed "ad hoc" (no Apple account needed), which is enough to
# run it on the Mac that built it.
#
# By default it is built for this Mac's processor. With UNIVERSAL=1 it is
# built for both Apple Silicon and Intel and the two are joined into one
# program that runs on either (needs `rustup target add aarch64-apple-darwin
# x86_64-apple-darwin`). The release workflow builds it that way.
set -eu

cd "$(dirname "$0")/.."
if [ "${UNIVERSAL:-0}" = 1 ]; then
    cargo build --release -p ale-term --target aarch64-apple-darwin
    cargo build --release -p ale-term --target x86_64-apple-darwin
    mkdir -p target/release
    lipo -create -output target/release/ale-term \
        target/aarch64-apple-darwin/release/ale-term \
        target/x86_64-apple-darwin/release/ale-term
else
    cargo build --release -p ale-term
fi

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
