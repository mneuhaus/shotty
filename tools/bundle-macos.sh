#!/bin/sh
# Build Shotty.app (release) into dist/ and sign it. --install copies it to /Applications.
#
# Signing uses a real identity (first "Apple Development" one in the keychain, or $SHOTTY_SIGN):
# macOS ties the Screen Recording permission to the signature, and an ad-hoc signature changes
# with every build, which would make macOS ask again after each rebuild.
set -eu
cd "$(dirname "$0")/.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP=dist/Shotty.app
IDENTITY=${SHOTTY_SIGN:-$(security find-identity -v -p codesigning | sed -n 's/.*"\(Apple Development[^"]*\)".*/\1/p' | head -1)}

cargo build --release -p shotty-app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/shotty "$APP/Contents/MacOS/Shotty"
cp crates/shotty-app/assets/Shotty.icns "$APP/Contents/Resources/Shotty.icns"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Shotty</string>
  <key>CFBundleDisplayName</key><string>Shotty</string>
  <key>CFBundleIdentifier</key><string>nrw.neuhaus.shotty</string>
  <key>CFBundleExecutable</key><string>Shotty</string>
  <key>CFBundleIconFile</key><string>Shotty</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>15.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSScreenCaptureUsageDescription</key><string>Shotty takes screenshots and screen recordings.</string>
  <key>NSMicrophoneUsageDescription</key><string>Shotty records your voice into screen recordings when you turn the microphone on.</string>
</dict>
</plist>
PLIST
xattr -cr "$APP"
if [ -n "$IDENTITY" ]; then
    codesign --force --sign "$IDENTITY" "$APP"
else
    echo "no signing identity found, signing ad hoc (Screen Recording permission resets on every build)" >&2
    codesign --force --sign - "$APP"
fi
codesign --verify --strict "$APP"
echo "built $APP ($VERSION, signed by ${IDENTITY:-ad hoc})"

if [ "${1:-}" = "--install" ]; then
    pkill -x Shotty 2>/dev/null || true
    rm -rf /Applications/Shotty.app
    cp -R "$APP" /Applications/Shotty.app
    echo "installed /Applications/Shotty.app"
fi
