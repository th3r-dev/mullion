# SRS: Mullion PoC 1, low-latency mirror to one tablet

Status: v1, 2026-11-09. Being built on the `feat/` branch; the roadmap is in [PLAN.md](PLAN.md).
Host for this PoC is **macOS** (see A-2); Windows stays the first host for M2.

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

- macOS host captures its main display and streams it to one Android tablet.
- Hardware H.264 encode on the host (VideoToolbox).
- The tablet app is a Dioxus Android app that plays the stream in a `<video>` element.
- Manual connection by typing the host IP on the tablet.
- A latency probe that measures glass-to-glass time.
- A desktop build of the same receiver app, so the whole pipeline can be run in
  loopback on the host machine before a tablet is attached.

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
- [str0m](https://docs.rs/str0m) is a sans-I/O Rust WebRTC stack at 0.25.0
  (3 Nov 2026). It lets the host write already-encoded H.264 frames per media
  line, so no software encoder or decoder enters the host process. Its one hard
  rule: every mutation of the `Rtc` is followed by a full `poll_output` drain, so
  one thread owns the `Rtc` and capture hands it frames over a channel.
- [Dioxus 0.7](https://github.com/DioxusLabs/dioxus/releases) ships Android
  support through the system WebView (`dx serve --platform android`); the newest
  stable is 0.7.10 (July 2026), which this PoC pins. No public report covers
  `<video>` with WebRTC inside it, which is part of what this PoC finds out.
- Chromium's `RTCRtpReceiver.jitterBufferTarget` (and the older `playoutDelayHint`)
  can drop the receive buffer close to zero; the Android System WebView is Chromium,
  so it should honor them.
- ScreenCaptureKit delivers one `CMSampleBuffer` per changed frame as an IOSurface-
  backed pixel buffer; VideoToolbox's `VTCompressionSession` takes that buffer
  directly, so no frame is copied through CPU memory. `RealTime`,
  `AllowFrameReordering=false` and `ProfileLevel=Baseline_AutoLevel` give the
  low-latency, no-B-frame stream WebRTC wants.
- 1080p60 is 489,600 macroblocks/s, which is H.264 level 4.2; a conformant encoder
  emits `profile-level-id` 42e02a, not Chrome's advertised 42e01f. The SDP
  negotiation is done on the level Chrome offers; the bitstream carries its own.
- Known failure modes that become tests: muted-autoplay rules in WebViews, B-frames
  adding a frame of delay, Wi-Fi power saving on tablets, the Screen Recording
  permission prompt, Retina scaling changing the capture size, and mixed-content
  blocks when a non-http WebView origin fetches a plain-http signaling URL.

## 4. Functional requirements

- **FR-1** The host shall capture the main display at its native resolution scaled to
  1920x1080 (letterboxed if the aspect differs) at up to 60 fps using ScreenCaptureKit.
- **FR-2** The host shall encode each frame with a hardware H.264 encoder: Constrained
  Baseline profile, no B-frames, level 4.2 or lower, CBR between 8 and 15 Mbps,
  an IDR frame on request.
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
  hardware H.264 encoder is present (`kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder`).
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
| A-2 | macOS is the host for PoC 1 | The question is the tablet decode path, which is host-independent. No Windows PC is on hand; this M1 Mac has ScreenCaptureKit and VideoToolbox. Windows stays the first host for M2 (step 2 needs IddCx). Only FR-1, FR-2, FR-8, A-5, A-9 changed |
| A-3 | str0m for host WebRTC | Accepts pre-encoded frames, no async runtime forced on us, maintained in 2026 |
| A-4 | Tablet side uses the WebView's JavaScript `RTCPeerConnection`, not Rust | The plan says to decode in the WebView, which comes with its own WebRTC stack |
| A-5 | VideoToolbox with hardware required | Apple's only public encoder API; the hardware flag turns the software fallback into a named error (FR-8) |
| A-6 | H.264 Constrained Baseline, packetization-mode 1 | The profile every WebRTC receiver must support |
| A-7 | Signaling over plain HTTP on the LAN, manual IP | Pairing and encryption of signaling arrive in step 3; WebRTC media is DTLS-SRTP already |
| A-8 | Port 47100 | Unassigned and easy to firewall; changeable by flag |
| A-9 | Reference hardware: an Apple Silicon Mac on Ethernet, a mid-range Android 13+ tablet, 5 GHz Wi-Fi 6 router | Matches the plan's network advice; results on other hardware are noted, not gated |
| A-10 | Tracker is GitHub Issues; PR against `dev` | No Jira MCP was connected when building started; `gh` is |
| A-11 | License `MIT OR Apache-2.0` | Matches str0m and Dioxus; keeps the App Store open; the GPL prior art (lan-mouse, Input Leap) is read for design only |
| A-12 | Signaling is also served over HTTPS with a self-signed certificate | A release WebView may run on a non-http origin, where a plain-http fetch is blocked as mixed content. Plain HTTP stays for `curl` and the loopback test |
| A-13 | Probe strip encodes host wall-clock milliseconds, 32 bits | Painted by a browser page the host serves, so the probe needs no native window; ms resolution is enough at 16.7 ms frames |
| A-14 | Loopback verification before on-device | No tablet is attached on the build machine. The desktop build of the receiver (WKWebView) runs the full pipeline on the host; on-device tests stay open until a tablet is plugged in |

## 7. Success criteria

The PoC **proves** the pipeline when, on the reference hardware, NFR-1 and NFR-2
both pass and FR-1 to FR-8 pass.

It **disproves** WebView decoding when NFR-1 fails and the per-stage log (FR-9)
puts more than 20 ms in the tablet's receive-to-present stage. The fallback then is
to decode with Android `MediaCodec` through JNI and draw to a native surface, which
changes the Android app design in [PLAN.md](PLAN.md) before step 2 starts.

If NFR-1 fails in the host stages instead, the fix is the encoder settings or a vendor
SDK (A-5), and the WebView design stands.
