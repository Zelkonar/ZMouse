#!/bin/sh
# Build zmouse as a macOS .app bundle (menu-bar agent), code-sign it, and verify it launches.
#
#   scripts/bundle.sh
#
# Output: dist/zmouse.app  — copy it to /Applications for stable, permanent use.
set -eu

REPO="$(cd "$(dirname "$0")/.." && pwd)"
APP="$REPO/dist/zmouse.app"
CONTENTS="$APP/Contents"

echo "Building release binary…"
( cd "$REPO" && cargo build --release --quiet )

echo "Assembling $APP …"
rm -rf "$APP"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"
cp "$REPO/target/release/zmouse" "$CONTENTS/MacOS/zmouse"
[ -f "$REPO/resources/AppIcon.icns" ] && cp "$REPO/resources/AppIcon.icns" "$CONTENTS/Resources/AppIcon.icns"

cat > "$CONTENTS/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>            <string>ZMouse</string>
    <key>CFBundleDisplayName</key>     <string>ZMouse</string>
    <key>CFBundleIdentifier</key>      <string>com.jeffreywolf.zmouse</string>
    <key>CFBundleExecutable</key>      <string>zmouse</string>
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

# Sign with a stable self-signed identity if a *valid* one is available (permissions then
# persist across rebuilds); otherwise fall back to ad-hoc (permissions reset each rebuild).
#
# The `-v` is load-bearing: an untrusted/invalid code-signing cert will still SIGN, and the
# result even passes `codesign --verify`, but the kernel SIGKILLs it at launch ("Code
# Signature Invalid"). `find-identity -v` lists only identities the system actually trusts,
# so we never sign with a cert that yields an unlaunchable bundle.
SIGN_ID="${ZMOUSE_SIGN_ID:-remouse-dev}"
if security find-identity -v -p codesigning 2>/dev/null | grep -q "\"$SIGN_ID\""; then
    echo "Code-signing with '$SIGN_ID' (stable identity — TCC permissions persist)…"
    codesign --force --deep --sign "$SIGN_ID" "$APP"
else
    echo "No *valid* signing identity '$SIGN_ID'; ad-hoc signing (permissions reset each rebuild)."
    echo "  For a stable identity: create a self-signed 'Code Signing' cert named '$SIGN_ID' in"
    echo "  Keychain Access AND set it to 'Always Trust' — an untrusted cert is SIGKILLed at launch."
    codesign --force --deep --sign - "$APP"
fi

# Smoke-test the signed bundle before declaring success. A bad signature (untrusted cert,
# tampered pages) sails past `codesign --verify` but is killed by the kernel the instant it
# execs, so the only reliable check is to actually run it. `list` is a quick, side-effect-free
# subcommand; a SIGKILL from an invalid signature shows up as exit 137 (128 + SIGKILL 9).
echo "Smoke-testing the signed bundle…"
if "$CONTENTS/MacOS/zmouse" list >/dev/null 2>&1; then
    echo "  OK — bundle launches."
else
    status=$?
    echo "ERROR: the signed bundle failed to launch (exit $status)." >&2
    [ "$status" -eq 137 ] && echo "  exit 137 = SIGKILL: the code signature was rejected at launch." >&2
    exit 1
fi

echo
echo "Built: $APP"
echo "Test it:      open \"$APP\""
echo "Install it:   cp -R \"$APP\" /Applications/ && open /Applications/zmouse.app"
echo
echo "First launch: grant Accessibility AND Input Monitoring in"
echo "System Settings -> Privacy & Security (add zmouse.app to both)."
