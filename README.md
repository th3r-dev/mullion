<p align="center"><img src="brand/logo.svg" width="320" alt="Mullion"></p>

<p align="center"><b>One desk, every screen.</b></p>

Mullion turns up to three Android tablets into extra monitors for a Windows, Mac or
Ubuntu PC over your local network, and moves one mouse and keyboard across all of
them. Written in Rust with [Dioxus](https://dioxuslabs.com).

**Status: planning.** No code yet. Start with [docs/PLAN.md](docs/PLAN.md).

## Three modes per device

| Mode | The device shows | Network |
| --- | --- | --- |
| Extend | Its own part of the desktop, on a virtual monitor | 8 to 15 Mbps |
| Mirror | A copy of the main screen; one encode shared by every mirroring device | 8 to 15 Mbps total |
| Control | Its own screen, driven by another device's mouse and keyboard across a screen edge | Input events only |

Switching mode never needs pairing again.

## How it works

```mermaid
flowchart LR
  subgraph PC["Host PC (Windows / macOS / Ubuntu)"]
    VM["Virtual monitor<br/>per tablet"] --> CAP["Capture"] --> ENC["Hardware H.264"]
    INJ["Input injection"]
  end
  ENC -- "WebRTC video" --> T1["Tablet 1<br/>WebView video"]
  ENC -- "WebRTC video" --> T2["Tablet 2"]
  ENC -- "WebRTC video" --> T3["Tablet 3"]
  T1 -- "touch, data channel" --> INJ
```

Only four parts differ by OS (virtual monitor, capture, encode, input), and they sit
behind one Rust trait. Pairing, transport, the UI and the Android app are shared.

| Part | Windows | macOS | Ubuntu |
| --- | --- | --- | --- |
| Virtual monitor | IddCx driver (MttVDD) | CGVirtualDisplay | Portal virtual monitor, evdi on X11 |
| Capture | Windows Graphics Capture | ScreenCaptureKit | PipeWire via portal |
| Encode | NVENC, Quick Sync, AMF | VideoToolbox | VA-API, NVENC |
| Input | SendInput | CGEvent | RemoteDesktop portal, uinput |

## Docs

| File | What it holds |
| --- | --- |
| [docs/PLAN.md](docs/PLAN.md) | Ten build steps, gates, repo layout, risks, open decisions |
| [docs/SRS.md](docs/SRS.md) | Spec for PoC 1: mirror one screen to one tablet under 50 ms |
| [docs/TEST_PLAN.md](docs/TEST_PLAN.md) | 29 tests covering every requirement in the SRS |
| [brand/BRAND.md](brand/BRAND.md) | Name, mark, color, type, motion, cursor label, voice |

## Targets

1080p at 60 fps per tablet, under 50 ms latency on 5 GHz Wi-Fi. For three tablets at
once, put the PC on Ethernet and the tablets on 5 GHz.
