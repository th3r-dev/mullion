# App fixes evidence

Captured on 2026-11-12 in an Android 15 emulator (arm64, WebView 124) on this Mac.

| File | What it shows |
|---|---|
| `android-connect-log.txt` | **Before**: the released beta.15 APK cannot reach any Mac. Its page is `https://dioxus.index.html`, and the WebView blocks every `http://` request as mixed content. **After**: this branch's APK pairs and connects through `mullionNative.http`, with ICE, DTLS and the data channel all up. |
| `android-app-update-link.png` | The app stamped `0.1.0-beta.14` offers "Get Mullion 0.1.0-beta.15" from GitHub, above its version. The Mac's host name in Nearby is replaced with `[Mac name]`. |
| `android-launcher-icon.png` | The launcher shows Mullion's icon (middle), not Android's template robot. |

Not captured: the Windows app. It uses the same page code: requests to the Mac go through the same native path, and the update check offers the `-windows-x64.exe` file.
