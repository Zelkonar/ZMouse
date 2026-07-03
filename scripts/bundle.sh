#!/bin/sh
# Build remouse as a macOS .app bundle (menu-bar agent) and ad-hoc code-sign it.
#
#   scripts/bundle.sh
#
# Output: dist/remouse.app  — copy it to /Applications for stable, permanent use.
set -eu

REPO="$(cd "$(dirname "$0")/.." && pwd)"
APP="$REPO/dist/remouse.app"
CONTENTS="$APP/Contents"

echo "Building release binary…"
( cd "$REPO" && cargo build --release --quiet )

echo "Assembling $APP …"
rm -rf "$APP"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"
cp "$REPO/target/release/remouse" "$CONTENTS/MacOS/remouse"
[ -f "$REPO/resources/AppIcon.icns" ] && cp "$REPO/resources/AppIcon.icns" "$CONTENTS/Resources/AppIcon.icns"

cat > "$CONTENTS/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>            <string>ZMouse</string>
    <key>CFBundleDisplayName</key>     <string>ZMouse</string>
    <key>CFBundleIdentifier</key>      <string>com.jeffreywolf.remouse</string>
    <key>CFBundleExecutable</key>      <string>remouse</string>
    <key>CFBundleVersion</key>         <string>0.1.0</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundlePackageType</key>     <string>APPL</string>
    <key>CFBundleIconFile</key>        <string>AppIcon</string>
    <key>LSMinimumSystemVersion</key>  <string>12.0</string>
    <!-- Menu-bar agent: no Dock icon, no default window. -->
    <key>LSUIElement</key>             <true/>
</dict>
</plist>
PLIST

printf 'APPL????' > "$CONTENTS/PkgInfo"

# Sign with a stable self-signed identity if available (permissions then persist across
# rebuilds); otherwise fall back to ad-hoc (permissions reset each rebuild).
SIGN_ID="${REMOUSE_SIGN_ID:-remouse-dev}"
if security find-identity -p codesigning 2>/dev/null | grep -q "\"$SIGN_ID\""; then
    echo "Code-signing with '$SIGN_ID' (stable identity — TCC permissions persist)…"
    codesign --force --deep --sign "$SIGN_ID" "$APP"
else
    echo "Signing identity '$SIGN_ID' not found; ad-hoc signing (permissions reset each rebuild)."
    echo "  Create a self-signed 'Code Signing' cert named '$SIGN_ID' in Keychain Access to fix."
    codesign --force --deep --sign - "$APP"
fi

echo
echo "Built: $APP"
echo "Test it:      open \"$APP\""
echo "Install it:   cp -R \"$APP\" /Applications/ && open /Applications/remouse.app"
echo
echo "First launch: grant Accessibility AND Input Monitoring in"
echo "System Settings -> Privacy & Security (add remouse.app to both)."
