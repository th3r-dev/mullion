#!/usr/bin/env bash
# Build Mullion.app and an installer package that puts it in /Applications and
# opens it when the install finishes.
#
#   tools/package-macos.sh
#       -> dist/macos/Mullion.app, dist/Mullion-<version>.pkg (ad-hoc signed)
#
# Signing (optional):
#   MULLION_SIGN_ID="Mullion Code Signing" tools/package-macos.sh
#       A stable signature: macOS keeps Screen Recording permission across
#       rebuilds and updates. With the ad-hoc default the app's identity is the
#       hash of this exact build, so every update asks again (and the old
#       "Mullion" entry in System Settings no longer applies).
#       Any certificate in the keychain works, a self-signed one too (CI
#       imports one from the MACOS_SIGN_P12 secret: release-beta.yml). Keep the
#       same one: a new certificate is a new identity to macOS.
#   MULLION_INSTALLER_ID="Developer ID Installer: Name (TEAMID)"
#       Signs the .pkg; with a Developer ID Application identity in
#       MULLION_SIGN_ID and notarization, it opens on other Macs without
#       a Gatekeeper warning.
#
# Version: MULLION_VERSION=0.1.0-beta.7 tools/package-macos.sh (CI sets it)
# builds that version into the app (shown on screen and compared by the update
# check) and names the package after it. Without it, the Cargo.toml version.
#
# The package is for Apple silicon (arm64), like the host build.
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=${MULLION_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}
export MULLION_VERSION="$VERSION"   # read at compile time (mullion_proto::version)
# Info.plist wants plain numbers there; the pre-release part stays in the app.
SHORT_VERSION=${VERSION%%-*}
BUILD=$(git rev-list --count HEAD 2>/dev/null || echo 1)
OUT=dist/macos
APP="$OUT/Mullion.app"
PKG="dist/Mullion-$VERSION.pkg"
SIGN_ID="${MULLION_SIGN_ID:--}"

echo "==> Building mullion-host $VERSION (build $BUILD)"
cargo build -p mullion-host --release

echo "==> Assembling $APP"
rm -rf "$OUT"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/mullion-host "$APP/Contents/MacOS/Mullion"
sed -e "s/__VERSION__/$SHORT_VERSION/" -e "s/__BUILD__/$BUILD/" crates/host/macos/Info.plist > "$APP/Contents/Info.plist"
plutil -lint -s "$APP/Contents/Info.plist"
cp crates/host/macos/Mullion.icns "$APP/Contents/Resources/Mullion.icns"
printf 'APPL????' > "$APP/Contents/PkgInfo"
# No extended attributes in the bundle: the package would carry them as
# AppleDouble ._ entries. (A protected com.apple.provenance may stay when this
# runs inside a sandboxed tool; the installer folds those back harmlessly.)
xattr -cr "$APP" 2>/dev/null || true

echo "==> Signing ($([ "$SIGN_ID" = "-" ] && echo ad-hoc || echo "$SIGN_ID"))"
sign=(codesign --force --sign "$SIGN_ID" --identifier app.mullion.host)
# Hardened runtime and a timestamp only for Apple-issued identities, which
# notarization needs; Apple's timestamp server does not serve self-signed ones.
case "$SIGN_ID" in "Developer ID"* | "Apple "*) sign+=(--options runtime --timestamp) ;; esac
"${sign[@]}" "$APP"
codesign --verify --strict --verbose=1 "$APP"

echo "==> Packaging $PKG"
ROOT="$OUT/root"
mkdir -p "$ROOT/Applications"
ditto --noextattr --noqtn "$APP" "$ROOT/Applications/Mullion.app"
# Not relocatable: always install to /Applications, never "upgrade" a copy
# that happens to sit somewhere else (such as this build folder).
pkgbuild --analyze --root "$ROOT" "$OUT/component.plist" >/dev/null
plutil -replace 0.BundleIsRelocatable -bool NO "$OUT/component.plist"
COPYFILE_DISABLE=1 pkgbuild --quiet --root "$ROOT" --component-plist "$OUT/component.plist" \
  --identifier app.mullion.host --version "$VERSION" --install-location / \
  --scripts tools/macos/scripts "$OUT/Mullion-component.pkg"
productbuild --synthesize --package "$OUT/Mullion-component.pkg" "$OUT/distribution.xml" >/dev/null
# The app icon in the Installer sidebar: the 256 px (144 dpi) rendition, so
# it draws crisp at 128 pt in the bottom-left corner, light and dark.
RES="$OUT/resources"
iconutil -c iconset -o "$OUT/Mullion.iconset" crates/host/macos/Mullion.icns
mkdir -p "$RES"
cp "$OUT/Mullion.iconset/icon_128x128@2x.png" "$RES/background.png"
rm -rf "$OUT/Mullion.iconset"
# Title, sidebar icon, Apple silicon only, install for this Mac, no customize step.
python3 - "$OUT/distribution.xml" <<'PY'
import sys, re
p = sys.argv[1]; s = open(p).read()
s = s.replace('<installer-gui-script minSpecVersion="1">',
  '<installer-gui-script minSpecVersion="1">\n    <title>Mullion</title>\n'
  '    <background file="background.png" alignment="bottomleft" scaling="none" mime-type="image/png"/>\n'
  '    <background-darkAqua file="background.png" alignment="bottomleft" scaling="none" mime-type="image/png"/>\n'
  '    <domains enable_anywhere="false" enable_currentUserHome="false" enable_localSystem="true"/>')
s = re.sub(r'<options [^>]*/>', '<options customize="never" require-scripts="false" hostArchitectures="arm64"/>', s)
open(p, "w").write(s)
PY
pkgsign=()
if [ -n "${MULLION_INSTALLER_ID:-}" ]; then pkgsign=(--sign "$MULLION_INSTALLER_ID"); fi
mkdir -p dist
productbuild --quiet --distribution "$OUT/distribution.xml" --package-path "$OUT" --resources "$RES" ${pkgsign[@]+"${pkgsign[@]}"} "$PKG"
rm -rf "$ROOT"
# The Mullion icon on the .pkg itself in Finder. It is stored in extended
# attributes, outside the signed archive: it survives zip, dmg and AirDrop,
# but a plain HTTP download drops it (the package still installs).
osascript -l JavaScript - "$PWD/crates/host/macos/Mullion.icns" "$PWD/$PKG" <<'JS' >/dev/null || echo "warning: could not set the package icon"
ObjC.import('AppKit');
function run(argv) {
  const icon = $.NSImage.alloc.initWithContentsOfFile(argv[0]);
  if (!$.NSWorkspace.sharedWorkspace.setIconForFileOptions(icon, argv[1], 0)) throw new Error('setIcon failed');
}
JS

echo
echo "Built:"
echo "  $APP"
echo "  $PKG ($(du -h "$PKG" | cut -f1))"
echo "Install: open $PKG   (or: sudo installer -pkg $PKG -target /)"
