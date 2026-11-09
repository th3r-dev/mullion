# PoC 1 report: low-latency mirror to one tablet

Date: 2026-11-09. Branch `feat/1-mirror-poc`, issue [#1](https://github.com/th3r-dev/mullion/issues/1).
Spec: [SRS.md](SRS.md). Tests: [TEST_PLAN.md](TEST_PLAN.md).

## The question (SRS 1)

Does the shared WebView-decode pipeline (hardware H.264 -> str0m WebRTC -> `<video>`
in a Chromium WebView with `jitterBufferTarget = 0`) hold 1080p60 under 50 ms
glass to glass?

## Answer so far

**Loopback: yes.** On the reference Mac (M1, macOS 27), host and a Chromium
receiver on the same machine, 2 minutes, continuous motion:

| Measure | Result | Gate |
| --- | --- | --- |
| Frames presented / decoded strip | 10,532 / 10,532 (0 failures) | |
| Glass to glass p50 | **26.8 ms** | |
| Glass to glass p95 | **43.7 ms** | NFR-1: under 50 ms |
| Host encode stage p95 (capture -> encoded) | 16.9 ms | FR-9 split |
| Encoded -> handed to WebRTC p95 | 0.14 ms | FR-9 split |
| `jitterBufferTarget` | supported, set to 0 | FR-5 |
| Peak 1 s send window | 9.1 Mbps at 10 Mbps target | NFR-4 |

Evidence: `logs/loopback-stats.json`, `logs/host-1794216133.jsonl`,
[evidence-loopback.png](evidence-loopback.png).

**Tablet over Wi-Fi: streams, number pending.** An Android tablet on the same
Wi-Fi connected through Chrome (`/receiver` page, same JS as the app) and showed
the Mac's display. Its `jitterBufferTarget` was honored (0). A clean 2-minute
p95 from the tablet has not been recorded yet: during the test window the host
was still single-session, so the loopback receiver and the tablet kept replacing
each other. The mixed host log over that window (tablet + loopback + several
reconnects) reads p50 40 ms, p95 124 ms, which is an upper bound, not the tablet
number. The multi-session host (commit `feat(host): several receivers share one
pipeline`) fixes the contention; the tablet run is the next thing to do.

**Dioxus app:** desktop build (WKWebView) runs; Android APK builds
(`target/dx/mullion-app/release/android/app/app/build/outputs/apk/debug/app-debug.apk`,
arm64). Neither has a recorded latency run yet. The stock Dioxus Android
manifest only permits cleartext HTTP to 127.0.0.1, which would have blocked the
signaling fetch on the LAN (the A-12 risk, confirmed); `uses_cleartext_traffic`
is now set in `Dioxus.toml`.

## Requirement status

| Req | Status | Evidence |
| --- | --- | --- |
| FR-1 capture, scaled, letterboxed, 60 fps | pass | 2560x1600 display -> 1920x1080 letterboxed; 54 fps average over a 285 s mixed window with mostly static desktop (SCK only sends changed frames; idle re-encode at 2 fps keeps the receiver alive) |
| FR-2 CBL, no B-frames, CBR 8-15 Mbps, IDR on request | pass (bitstream) / partial (bitrate) | VideoToolbox session: ConstrainedBaseline_AutoLevel, AllowFrameReordering=false; IDR on join and on PLI. Average bitrate was 0.79 Mbps on a static desktop; peak 1 s window 9.1 Mbps under motion. T-7's "average 8 to 15" needs a full-motion run |
| FR-3 signaling on 47100 | pass | T-9, T-10, T-11 unit tests |
| FR-4 connect by IP, video within 3 s, muted autoplay | pass | loopback and tablet; T-12, T-13 by observation |
| FR-5 jitterBufferTarget 0, recorded | pass | capabilities records in the log, every receiver |
| FR-6 PLI -> IDR within 200 ms, at most 2/s | partial | T-18 unit test passes; no PLI was observed in loopback (no loss), so T-17 is untested |
| FR-7 disconnect frees capture/encoder within 2 s | pass | pipeline stops when the last session ends (`session ended` events) |
| FR-8 named error without hardware encoder | pass | `--check-encoder` reports hardware=true; RequireHardware flag turns software fallback into "No hardware H.264 encoder found" |
| FR-9 per-stage timing | pass | JSON lines log, split computed above |
| NFR-1 p95 under 50 ms | pass in loopback; tablet pending | 43.7 ms |
| NFR-2 57+ fps presented, under 1 % dropped | partial | 10,532 frames in 120 s = 87.8 callbacks/s (the probe animates faster than 60); sampler missed 844 compositor frames (8 %); decode drops not separable in this build |
| NFR-3 host CPU under 15 % | not measured | |
| NFR-4 8-15 Mbps during motion | partial | 9.1 Mbps peak window; no sustained full-motion run |

Tests run: 25 unit tests in `proto`, 6 in `host` (T-9, T-10, T-11, timing log, T-18 via `IdrLimiter`), all passing.

## Assumptions made while building (SRS 6)

A-2 macOS host, A-10 GitHub Issues, A-11 MIT OR Apache-2.0, A-12 cleartext on
Android, A-13 probe burned into the frame by the host (replaced the on-screen
probe page: the page kept falling behind other windows, and the burned strip
measures exactly capture to glass), A-14 loopback first. Two more taken during
the build:

- **A-15** The host accepts several receivers sharing one pipeline. The SRS says
  one tablet; one-at-a-time made tablet and loopback testing fight each other.
  This is step 8's Mirror behavior arriving early; nothing in the gate depends on it.
- **A-16** Screen Recording permission belongs to the terminal that launches the
  host, not to the binary. `tools/run-host.sh` exists for that reason.

## Not proven here

- The tablet number over Wi-Fi (the actual NFR-1 gate; loopback excludes the radio).
- The Dioxus Android WebView specifically (Chrome on the tablet was used; same
  Chromium decoder, different process and security policy).
- Loss recovery (T-17), Wi-Fi power saving (T-25), camera cross-check (T-26),
  host CPU (T-28), sustained bitrate (T-29).

## Decision

Loopback says the WebView-decode design holds with 6 ms to spare at p95, and the
host-side split shows the encoder (17 ms p95) is the largest stage, not the
receiver. Unless the tablet run shows more than ~20 ms in the receive-to-present
stage (the SRS 7 disproof condition), the MediaCodec fallback is not needed and
step 2 can start.


## Addendum, same day: step 2 on macOS and the first per-device Wi-Fi numbers

Extend mode shipped on `feat/3-extend-macos` (issue #3): one `CGVirtualDisplay`
per device, named after the device in Display Settings, pipelines keyed by
capture source, Mirror/Extend toggle in every receiver, `/devices` endpoint.
Two tablets ran as two separate extended screens with the MacBook staying main.

A trap worth recording: macOS made the first virtual display *main* and turned
the built-in display into a mirror of it, so clicks landed on the tablet's
screen and the tablet looked like a mirror. The host now un-mirrors and pins the
built-in display as main after every creation (`platform/macos/arrange.rs`).

**Per-device glass to glass, both on Wi-Fi, same host, same minute** (short
sample, `logs/host-1794226196.jsonl`):

| Device | Receiver | n | p50 | p95 | max |
| --- | --- | --- | --- | --- | --- |
| Windows PC, 10.43.77.138 | Chrome 156 | 174 | 61 ms | **72 ms** | 525 ms |
| Tablet, 10.43.77.59 | Chrome 152, desktop mode | 514 | 159 ms | **621 ms** | 3,421 ms |

Host encode stage was 9 to 10 ms for both. The pipeline is identical up to the
radio, so the 9x gap is the device or its link, not the design. Before the SRS
7 disproof condition ("more than 20 ms in receive-to-present") can be judged for
the tablet, it needs: 5 GHz confirmed, Wi-Fi power saving off (T-25), Chrome in
mobile mode, and ideally the Mullion APK instead of the browser. The Windows
PC's 72 ms is over the 50 ms gate too, by 22 ms, with the Mac itself on Wi-Fi
rather than the Ethernet the reference setup (A-9) assumes.
