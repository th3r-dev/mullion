# SRS: Mullion PoC 1, low-latency mirror to one tablet

Source plan: [Android Tablet Extended Screen App, Dioxus Architecture Plan](https://claude.ai/artifact/Y69wyvkwBqCpKfPfMJ7g6f) (2026-11-09).
Status: spec only, nothing built. The full roadmap is in [PLAN.md](PLAN.md).

## 1. Purpose

Mullion's whole product rests on one pipeline: capture a screen, encode it in
hardware, send it over WebRTC, and play it inside the Dioxus Android app with the
WebView's own `<video>` decoder. If that pipeline cannot hold 1080p at 60 fps
under 50 ms glass to glass on 5 GHz Wi-Fi, Extend mode and Mirror mode both fail
and the decoder choice has to change before anything else is built.

Two things are uncertain. A Rust WebRTC sender (str0m) feeding the Android System
WebView has no published latency numbers. Dioxus mobile is younger than Dioxus
desktop, and its WebView may not expose the low-latency playout controls.
This PoC answers one question: **does the shared WebView-decode pipeline meet
the plan's latency target?** It is build step 1 of the plan.

## 2. Scope

In scope:

- Windows host captures its main monitor and streams it to one Android tablet.
- Hardware H.264 encode on the host.
- The tablet app is a Dioxus Android app that plays the stream in a `<video>` element.
- Manual connection by typing the host IP on the tablet.
- A latency probe that measures glass-to-glass time.

Out of scope (later build steps in [PLAN.md](PLAN.md)):

- Virtual monitors and Extend mode (step 2)
- mDNS discovery and PIN pairing (step 3); this PoC runs on a trusted LAN only
- More than one tablet, NVENC session limits (step 4)
- Touch input, USB wired mode (step 5)
- macOS and Linux hosts (steps 6, 7)
- Control mode, cursor labels, hotkeys (steps 8 to 10)
- Audio

## 3. Background / prior art

- Every extend-screen product (spacedesk, Duet, Sidecar) streams a virtual monitor;
  mirroring the real monitor first lets us measure the transport without the driver.
- [str0m](https://docs.rs/str0m) is a sans-I/O Rust WebRTC stack at 0.23.x, last
  pushed April 2026. It lets the host write already-encoded H.264 frames per
  media line, so no software encoder or decoder enters the host process.
- [Dioxus 0.7](https://github.com/DioxusLabs/dioxus/releases/tag/v0.7.0) ships Android
  support through the system WebView (`dx serve --platform android`); the newest
  release seen is 0.7.9 (May 2026). No public report covers `<video>` with WebRTC
  inside it, which is part of what this PoC finds out.
- Chromium's `RTCRtpReceiver.jitterBufferTarget` (and the older `playoutDelayHint`)
  can drop the receive buffer close to zero; the Android System WebView is Chromium,
  so it should honor them.
- Windows Graphics Capture gives one GPU texture per changed frame. Media Foundation's
  hardware H.264 encoder covers NVENC, Quick Sync and AMF behind one API, with
  `CODECAPI_AVLowLatencyMode` for low latency.
- Known failure modes that become tests: muted-autoplay rules in WebViews, B-frames
  adding a frame of delay, Wi-Fi power saving on tablets, the secure desktop (UAC)
  that capture cannot see, and DPI scaling changing the capture size.

## 4. Functional requirements

- **FR-1** The host shall capture the main monitor at its native resolution scaled to
  1920x1080 (letterboxed if the aspect differs) at up to 60 fps using Windows Graphics Capture.
- **FR-2** The host shall encode each frame with a hardware H.264 encoder: Constrained
  Baseline profile, no B-frames, CBR between 8 and 15 Mbps, an IDR frame on request.
- **FR-3** The host shall run a signaling endpoint on TCP port 47100 that accepts a
  WebRTC SDP offer from the tablet and returns an answer.
- **FR-4** The tablet app shall connect after the user types the host IP, and show the
  stream in a muted, autoplaying `<video>` element within 3 s of tapping Connect.
- **FR-5** The tablet app shall set the receiver's jitter buffer target to 0 ms where
  the WebView supports it, and record whether it did.
- **FR-6** When the tablet reports packet loss with a PLI, the host shall send an IDR
  frame within 200 ms.
- **FR-7** When the tablet disconnects or closes the app, the host shall stop capture
  and release the encoder within 2 s.
- **FR-8** The host shall fail with a named error, and no CPU-encode fallback, when no
  hardware H.264 encoder is present.
- **FR-9** Both sides shall log per-frame timing (capture, encode done, send, frame
  presented) so latency can be split by stage.

## 5. Non-functional requirements

- **NFR-1 Latency.** Glass-to-glass latency p95 under 50 ms, host on Ethernet,
  tablet on 5 GHz Wi-Fi, 1080p60 (target from the plan).
- **NFR-2 Frame rate.** At least 57 fps presented on average over 5 minutes of
  continuous motion, with under 1% of frames dropped.
- **NFR-3 Host load.** Host process CPU under 15% of one machine at 1080p60, showing
  the encode is really on the GPU.
- **NFR-4 Bitrate.** Measured send rate stays between 8 and 15 Mbps during motion.

## 6. Assumptions

| # | Decision | Why |
| --- | --- | --- |
| A-1 | Product name Mullion, repo `mullion` | No clash found in screen or input sharing; trademark search still to do |
| A-2 | Windows is the first host | The plan lists Windows first and adds macOS and Linux in steps 6 and 7. If no Windows test machine is available, swap to macOS (ScreenCaptureKit + VideoToolbox); the SRS changes only in FR-1, FR-2, FR-8 |
| A-3 | str0m for host WebRTC | Accepts pre-encoded frames, no async runtime forced on us, maintained in 2026 |
| A-4 | Tablet side uses the WebView's JavaScript `RTCPeerConnection`, not Rust | The plan says to decode in the WebView, which comes with its own WebRTC stack |
| A-5 | Media Foundation hardware MFT instead of three vendor SDKs | One API covers NVENC, Quick Sync and AMF; vendor SDKs only if MF misses the latency target |
| A-6 | H.264 Constrained Baseline, packetization-mode 1 | The profile every WebRTC receiver must support |
| A-7 | Signaling over plain HTTP on the LAN, manual IP | Pairing and encryption of signaling arrive in step 3; WebRTC media is DTLS-SRTP already |
| A-8 | Port 47100 | Unassigned and easy to firewall; changeable by flag |
| A-9 | Reference hardware: a Windows 11 PC with an NVIDIA RTX GPU on Ethernet, a mid-range Android 13+ tablet, 5 GHz Wi-Fi 6 router | Matches the plan's network advice; results on other hardware are noted, not gated |
| A-10 | No tracker ticket and no PR yet | The request was for plans only. Jira is connected and will be the tracker when building starts |

## 7. Success criteria

The PoC **proves** the pipeline when, on the reference hardware, NFR-1 and NFR-2
both pass and FR-1 to FR-8 pass.

It **disproves** WebView decoding when NFR-1 fails and the per-stage log (FR-9)
puts more than 20 ms in the tablet's receive-to-present stage. The fallback then is
to decode with Android `MediaCodec` through JNI and draw to a native surface, which
changes the Android app design in [PLAN.md](PLAN.md) before step 2 starts.

If NFR-1 fails in the host stages instead, the fix is the encoder settings or a vendor
SDK (A-5), and the WebView design stands.
