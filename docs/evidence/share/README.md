# Share this screen evidence

Captured on 2026-11-12 in an Android 15 emulator (arm64) on the Mac, with this branch's APK.

| File | What it shows |
|---|---|
| `tablet-sharing.png` | After Android's consent, the app shows what to do on the Mac and the address to enter; "Stop sharing" ends it. Android's cast icon is in the status bar. |
| `tablet-notification.png` | The foreground service's notification, "Mullion is sharing this screen", which Android requires for screen capture. |
| `tablet-host-log.txt` | The tablet's host: `/hello` says Android and Mirror only. Pairing, then a receiver's offer is answered, and ICE, DTLS and the data channel come up. The receiver decodes 1072×1920 frames. After Stop sharing, offers are refused. |

**What the emulator can't show:** a Mac viewing the tablet. The Mac can't reach the emulator's UDP ports, so the receiver here is the app's own page, connected over loopback. Decoding its own screen in software is also what eventually runs the emulator's WebView out of memory. On real devices the Mac decodes, and the tablet only encodes in hardware.
