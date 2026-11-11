#!/usr/bin/env bash
# Build the Mullion tablet APK (arm64, debug-signed) into dist/.
# dx 0.7.10 names the app "MullionApp", gives it Android's template icon,
# generates a network_security_config that only allows cleartext to 127.0.0.1
# (PoC 1 signaling is plain HTTP on the LAN, SRS A-12), and has no screen
# sharing. All are replaced or added, and Gradle re-run before copying the APK out.
set -euo pipefail
cd "$(dirname "$0")/.."
export ANDROID_HOME="${ANDROID_HOME:-/opt/homebrew/share/android-commandlinetools}"
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$ANDROID_HOME/ndk/$(ls "$ANDROID_HOME/ndk" | sort -V | tail -1)}"
export JAVA_HOME="${JAVA_HOME:-/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home}"

APP=target/dx/mullion-app/release/android/app
# A rerun: dx writes its template icons (.webp) next to the last build's
# Mullion icons (.png) and Gradle refuses the duplicates. Take ours out first.
rm -f "$APP"/app/src/main/res/mipmap-*/ic_launcher*.png
(cd crates/app && dx build --platform android --release --features mobile --no-default-features --target aarch64-linux-android)

cat > "$APP/app/src/main/res/xml/network_security_config.xml" <<'EOF'
<?xml version="1.0" encoding="utf-8"?>
<network-security-config>
    <!-- PoC 1: signaling is plain HTTP on the LAN (SRS A-7, A-12). -->
    <base-config cleartextTrafficPermitted="true" />
</network-security-config>
EOF
# The launcher icon: dx ships Android's template robot. Replace it with
# Mullion's (crates/app/android/res, from tools/make-icons.py --android).
rm -f "$APP"/app/src/main/res/mipmap-*/ic_launcher.webp "$APP"/app/src/main/res/mipmap-*/ic_launcher_round.webp
cp -R crates/app/android/res/. "$APP/app/src/main/res/"
# The launcher name: dx derives "MullionApp" from the crate name.
cat > "$APP/app/src/main/res/values/strings.xml" <<'EOF'
<resources>
    <string name="app_name">Mullion</string>
</resources>
EOF
# "Share this screen" (crates/app/src/share.rs): Mullion's MainActivity over
# dx's empty one, the capture service, and what Android asks of it: a
# foreground service of type mediaProjection and its permissions.
cp crates/app/android/kotlin/MainActivity.kt crates/app/android/kotlin/Share.kt "$APP/app/src/main/kotlin/dev/dioxus/main/"
MANIFEST="$APP/app/src/main/AndroidManifest.xml"
grep -q ShareService "$MANIFEST" || perl -0pi -e '
  s{(<uses-permission android:name="android.permission.INTERNET" />)}{$1
    <uses-permission android:name="android.permission.FOREGROUND_SERVICE" />
    <uses-permission android:name="android.permission.FOREGROUND_SERVICE_MEDIA_PROJECTION" />
    <uses-permission android:name="android.permission.POST_NOTIFICATIONS" />};
  s{(\s*</application>)}{
        <service android:name="dev.dioxus.main.ShareService" android:foregroundServiceType="mediaProjection" android:exported="false" />$1};
' "$MANIFEST"
grep -q ShareService "$MANIFEST" || { echo "AndroidManifest.xml: could not add ShareService (dx changed its template?)" >&2; exit 1; }
(cd "$APP" && ./gradlew assembleDebug -q)
mkdir -p dist
cp "$APP/app/build/outputs/apk/debug/app-debug.apk" dist/mullion-tablet-debug.apk
echo "APK: dist/mullion-tablet-debug.apk"
echo "Install: adb install -r dist/mullion-tablet-debug.apk  (or copy it to the tablet and open it)"
