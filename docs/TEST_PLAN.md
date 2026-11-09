# Test plan: Mullion PoC 1

Every requirement in [SRS.md](SRS.md) has a happy-path test and at least one edge
or failure test. Written before any code; tests become `cargo test` targets and one
on-device script when building starts.

## How latency is measured (NFR-1)

0. Start the host with `--probe` (or `MULLION_PROBE=true`); the strip is off by
   default so it does not cover the picture in normal use.
1. The host serves a probe page (`/probe`) that is opened full screen on the main
   display. Every frame it paints a strip of 32 black and white blocks encoding the
   host's wall-clock time in milliseconds (low 32 bits), two parity blocks and two
   tail blocks that mark the strip's right edge (38 blocks).
2. The tablet reads that strip from each presented frame: `requestVideoFrameCallback`
   gives the frame and its `expectedDisplayTime`; the strip is sampled from a 1-pixel
   high canvas copy of the video.
3. Clocks are aligned with an NTP-style ping over a WebRTC data channel (20 pings,
   keep the one with the lowest round trip).
4. Glass to glass = tablet display time minus host paint time, after the clock offset.
5. Cross-check once by filming both screens with a phone at 240 fps. The two methods
   should agree within 8 ms; if not, trust the camera and find out why.

## Test matrix

| Test ID | Requirement | Scenario | Type | Expected result |
| --- | --- | --- | --- | --- |
| T-1 | FR-1 | 1920x1080 main monitor, moving content | integration (host) | Frames arrive at 60 fps, 1920x1080 |
| T-2 | FR-1 | 2560x1600 Retina display (16:10) | integration (host) | Output is 1920x1080, aspect kept, letterboxed, no crop |
| T-3 | FR-1 | Resolution changed while streaming | integration (host) | Stream continues within 1 s at the new size, no crash |
| T-4 | FR-1 | Screen Recording permission not granted | manual | Host exits with the BRAND.md permission message naming the Settings pane, no crash |
| T-5 | FR-1 | Static screen, nothing changes for 10 s | integration (host) | Host keeps sending at least 1 fps so the tablet does not time out |
| T-6 | FR-2 | Encode 600 frames, inspect the bitstream | unit (host) | SPS profile_idc 66 with constraint_set1 (Constrained Baseline), level_idc at most 42, zero B-frames, IDR at frame 0 |
| T-7 | FR-2 | Bitrate over 60 s of motion | integration (host) | Average 8 to 15 Mbps, no 1 s window above 20 Mbps |
| T-8 | FR-2 | Request IDR mid-stream | unit (host) | Next encoded frame is an IDR |
| T-9 | FR-3 | Valid SDP offer posted to port 47100 | unit (host) | 200 with an SDP answer offering H.264 packetization-mode 1 |
| T-10 | FR-3 | Malformed body, 2 MB body, wrong method | unit (host) | 400, 413, 405; host keeps running |
| T-11 | FR-3 | Port 47100 already in use | unit (host) | Startup fails with a message naming the port and the flag to change it |
| T-12 | FR-4 | Type host IP, tap Connect | on-device | Video visible within 3 s |
| T-13 | FR-4 | WebView blocks unmuted autoplay | on-device | Video is muted and plays with no tap |
| T-14 | FR-4 | Wrong IP, host off | on-device | Clear "Can't reach 192.168.1.20" message within 5 s, app usable |
| T-15 | FR-4 | Tablet rotates to portrait mid-stream | on-device | Video letterboxes, stream continues |
| T-16 | FR-5 | Read receiver settings after connect | on-device | Jitter buffer target is 0, or the log says the WebView lacks support |
| T-17 | FR-6 | Inject 5% packet loss (router or `dnctl`/`pfctl`) | integration | PLI seen on host, IDR sent within 200 ms, picture recovers within 1 s |
| T-18 | FR-6 | 30 PLIs in 1 s | unit (host) | IDRs rate-limited to at most 2 per second |
| T-19 | FR-7 | Tablet app closed | integration | Host stops capture and frees the encoder within 2 s |
| T-20 | FR-7 | Tablet Wi-Fi turned off (no goodbye) | integration | Host notices via ICE timeout and frees the encoder within 10 s |
| T-21 | FR-7 | Reconnect right after a disconnect, 10 times | integration | Every reconnect works; host memory and GPU sessions do not grow |
| T-22 | FR-8 | No hardware encoder (VM, or `--require-hardware` on a session the OS refuses) | integration (host) | Exits with "No hardware H.264 encoder found", no CPU encode |
| T-23 | FR-9 | Run 60 s | integration | Each frame has capture, encode, send and present times in the log |
| T-24 | NFR-1 | Reference setup, probe window, 5 min | on-device | p95 glass to glass under 50 ms |
| T-25 | NFR-1 | Same, with the tablet's Wi-Fi power saving on | on-device | Result recorded; if p95 fails, the app must hold a Wi-Fi lock |
| T-26 | NFR-1 | Camera cross-check | manual | Within 8 ms of T-24 |
| T-27 | NFR-2 | Full-screen motion video, 5 min | on-device | At least 57 fps presented, under 1% dropped |
| T-28 | NFR-3 | Same run as T-27 | integration (host) | Host process CPU under 15% |
| T-29 | NFR-4 | Same run as T-27 | integration | Send rate 8 to 15 Mbps |

## Coverage check

| Requirement | Tests |
| --- | --- |
| FR-1 | T-1 to T-5 |
| FR-2 | T-6 to T-8 |
| FR-3 | T-9 to T-11 |
| FR-4 | T-12 to T-15 |
| FR-5 | T-16 |
| FR-6 | T-17, T-18 |
| FR-7 | T-19 to T-21 |
| FR-8 | T-22 |
| FR-9 | T-23 |
| NFR-1 | T-24 to T-26 |
| NFR-2 | T-27 |
| NFR-3 | T-28 |
| NFR-4 | T-29 |

FR-5 has one test because its failure path (no support) is part of that same test.

## Loopback stage (A-14)

Before a tablet is attached, T-1, T-6 to T-11, T-18, T-19, T-21, T-23 run on the
host alone, and T-12 to T-16, T-24, T-27 to T-29 run with the desktop build of the
receiver on the same Mac. Loopback numbers prove the pipeline works; they do not
replace the on-device numbers for the NFR-1 gate.
