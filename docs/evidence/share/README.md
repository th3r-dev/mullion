# Share this screen evidence

Captured on 2026-11-12 in an Android 15 emulator (arm64) on the Mac, with this branch's APK.

| File | What it shows |
|---|---|
| `tablet-sharing.png` | After Android's consent, the app shows what to do on the Mac and the address to enter; "Stop sharing" ends it. Android's cast icon is in the status bar. |
| `tablet-notification.png` | The foreground service's notification, "Mullion is sharing this screen", which Android requires for screen capture. |
| `tablet-host-log.txt` | The tablet's host: `/hello` says Android and Mirror only. Pairing, then a receiver's offer is answered, and ICE, DTLS and the data channel come up. The receiver decodes 1072×1920 frames. After Stop sharing, offers are refused. |

**What the emulator can't show:** a Mac viewing the tablet. The Mac can't reach the emulator's UDP ports, so the receiver here is the app's own page, connected over loopback. Decoding its own screen in software is also what eventually runs the emulator's WebView out of memory. On real devices the Mac decodes, and the tablet only encodes in hardware.

## Update prompt and full screen

| File | What it shows |
|---|---|
| `app-update-prompt.png` | A build stamped `0.1.0-beta.14` asks on start: "Mullion 0.1.0-beta.19 is out. You have 0.1.0-beta.14", with **Later** and **Update**. Update opened Chrome with the APK's github.com URL (logcat: `START u0 {act=android.intent.action.VIEW dat=https://github.com/... cmp=com.android.chrome/...}` from Mullion's uid). |
| `android-fullscreen.png` | Full screen on Android hides the status and navigation bars. The page grows from 412×684 to 412×732 CSS px (the whole 1080×1920 screen) and goes back on exit. |

The Mac's host name in Nearby is replaced with `[Mac name]`. Windows full screen was not run here; there's no Windows machine.
