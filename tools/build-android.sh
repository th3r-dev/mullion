#!/usr/bin/env bash
# Build the Mullion tablet APK (arm64, debug-signed) into dist/.
# dx 0.7.10 names the app "MullionApp" and generates a network_security_config
# that only allows cleartext to 127.0.0.1; PoC 1 signaling is plain HTTP on the
# LAN (SRS A-12). Both are replaced and Gradle re-run before copying the APK out.
set -euo pipefail
cd "$(dirname "$0")/.."
export ANDROID_HOME="${ANDROID_HOME:-/opt/homebrew/share/android-commandlinetools}"
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$ANDROID_HOME/ndk/$(ls "$ANDROID_HOME/ndk" | sort -V | tail -1)}"
export JAVA_HOME="${JAVA_HOME:-/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home}"

(cd crates/app && dx build --platform android --release --features mobile --no-default-features --target aarch64-linux-android)

APP=target/dx/mullion-app/release/android/app
cat > "$APP/app/src/main/res/xml/network_security_config.xml" <<'EOF'
<?xml version="1.0" encoding="utf-8"?>
<network-security-config>
    <!-- PoC 1: signaling is plain HTTP on the LAN (SRS A-7, A-12). -->
    <base-config cleartextTrafficPermitted="true" />
</network-security-config>
EOF
# The launcher name: dx derives "MullionApp" from the crate name.
cat > "$APP/app/src/main/res/values/strings.xml" <<'EOF'
<resources>
    <string name="app_name">Mullion</string>
</resources>
EOF
(cd "$APP" && ./gradlew assembleDebug -q)
mkdir -p dist
cp "$APP/app/build/outputs/apk/debug/app-debug.apk" dist/mullion-tablet-debug.apk
echo "APK: dist/mullion-tablet-debug.apk"
echo "Install: adb install -r dist/mullion-tablet-debug.apk  (or copy it to the tablet and open it)"
