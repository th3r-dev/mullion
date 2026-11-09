# Mullion brand

Status: v0 draft, 2026-11-09. Name and mark are proposals until confirmed.

## Name

A mullion is the bar between two panes of a window. Mullion turns tablets and
computers into panes of one desk, and the cursor crosses the mullion between
them. One word, easy to say in English and Thai (มัลเลียน), not taken by a
screen or input-sharing product we could find. A trademark search has not been
done yet.

Tagline: **One desk, every screen.**

Use "Mullion" with a capital M in prose and `mullion` for the repo, crates and
package IDs (`app.mullion.tablet`).

## Mark

![mark](mark.svg) `mark.svg` is the app icon and favicon. `logo.svg` is the mark plus wordmark.

- Two panes: the left one is the computer you sit at (ink), the right one is the
  screen you reach into (blue). The cursor tip sits on the gap, about to cross.
- Both SVGs switch to their dark variant through `prefers-color-scheme`.
- Keep clear space of one pane width (26 units) around the mark.
- Below 24 px the cursor becomes noise; the two panes alone still read. Use the
  mark as is down to 16 px; draw a cursor-less favicon only if testing shows a problem.
- The wordmark is live `system-ui` text. Convert it to paths before using the
  logo outside this repo (store pages, slides).

## Color

Two core colors plus one accent. Device colors are functional, not decoration.

| Token | Light | Dark | Use |
| --- | --- | --- | --- |
| `--ink` | `#111418` | `#ECEDEE` | Text, the "this computer" pane |
| `--paper` | `#F7F7F5` | `#0F1113` | Page and window background |
| `--line` | `#D9DBDE` | `#2A2E33` | Dividers, tile borders |
| `--accent` | `#1F5FD6` | `#5B93FF` | The one primary action per view, the active pane, focus ring |
| `--on-accent` | `#FFFFFF` | `#0F1113` | Text on an `--accent` fill |
| `--device-1` | `#D9532B` | `#FF7A52` | Device 1 dot and tile edge |
| `--device-2` | `#138A77` | `#3CC7AE` | Device 2 dot and tile edge |
| `--device-3` | `#8A4CCB` | `#B98CF0` | Device 3 dot and tile edge |

Rules:

- Measured contrast: `--accent` on `--paper` is 5.3:1 light and 6.4:1 dark;
  `--on-accent` on `--accent` is 5.7:1 light and 6.4:1 dark. White on the dark
  accent is only 2.97:1, which is why `--on-accent` flips. Check any new pair before using it.
- Device colors never carry text. They appear as an 8 px dot, a 2 px tile edge on
  the layout map, and the ring on the cursor label. Against `--paper` they measure
  3.7 to 9.0:1, above the 3:1 needed for non-text marks.
- The cursor label surface is always dark, so its dot and ring always use the
  dark-column device colors (7.1:1 and up), whatever the OS theme. The label text
  is white on that surface (18:1), legible on any wallpaper.
- Users can change a device color; the app picks from these three first, then
  lets them choose any color that passes 3:1 against `--paper` as a non-text mark.
- Light and dark are both first-class. The app follows the OS setting.

## Type

The platform font, nothing custom: SF Pro on macOS, Segoe UI Variable on
Windows, Ubuntu on Linux, Roboto on Android (`font-family: system-ui`). They ship
optical sizing and tracking tables already.

| Role | Size | Weight | Line height | Tracking |
| --- | --- | --- | --- | --- |
| Display (pairing code) | 40 px | 650 | 1.05 | -0.02em |
| Title | 22 px | 650 | 1.2 | -0.01em |
| Body | 15 px | 400 | 1.5 | 0 |
| Label (cursor badge, tiles) | 12 px | 600 | 1.2 | +0.01em |

Pairing codes and device numbers use tabular figures (`font-variant-numeric: tabular-nums`).
Sizes are in `rem` in code so they follow the OS text size.

## The cursor label

The most visible piece of brand, because it shows up on other people's screens.

```
 ┌───────────────────┐
 │ ● DAISY-PC (1)    │   dark pill, 12 px label, device-colored dot and 1 px ring
 └───────────────────┘
   ↖ cursor
```

- Surface: `rgba(17,20,24,0.86)` with `backdrop-filter: blur(12px)`, radius 6 px,
  padding 3 px 8 px, offset 14 px right and 18 px below the cursor hotspot. This is
  one of the two places glass is allowed (the other is the switch toast).
- Text: device name and number, white. Names default to the hostname.
- Shown the whole time a device is being controlled. After 2 s without motion it
  dims to 60% opacity; it returns to full on the next move.
- `prefers-reduced-transparency`: solid `#111418`, no blur.
- `prefers-contrast: more`: add a 1 px white outline.

## Motion

Springs, not durations, for anything a person can touch. Values follow Apple's
published numbers (damping ratio, response in seconds).

| Interaction | Behavior |
| --- | --- |
| Buttons, toggles | Respond on pointer-down: scale 0.97 within 100 ms. Commit on release. |
| Panels, sheets, mode switch | Spring, damping 1.0, response 0.35. No overshoot. |
| Dragging a device tile on the layout map | 1:1 with the pointer, keeping the grab offset. On release, project the landing point with deceleration 0.998, snap to the nearest free edge, spring damping 0.8, response 0.3, handing over release velocity. Past the map bounds, rubber-band (constant 0.55). |
| Cursor label entering | Materializes at the entry edge: scale 0.92 to 1, blur 6 px to 0, opacity 0 to 1, spring damping 1.0, response 0.25. |
| Switch toast on the source device | Slides in from the edge the cursor left through and leaves the same way. Stays 1.6 s. |
| Control arriving on Android | One light haptic tick (`HapticFeedbackConstants.CONFIRM`), same frame as the label. |

`prefers-reduced-motion`: every slide and spring becomes a 150 ms opacity fade.
The cursor label appears without the blur and scale. Nothing loops.

## Voice

Plain, specific, short. Say what happened and what the person can do. Use device
names, never "the remote device".

| Moment | Write | Not |
| --- | --- | --- |
| Pairing | Enter 482 913 on DAISY-PC | Please enter the verification code to continue |
| Switch | Now controlling MacBook-Air (2) | Successfully switched devices! |
| Lost link | Lost DAISY-PC. Mouse and keyboard are back on this tablet. | An error occurred. |
| Permission | Mullion needs Screen Recording to show this Mac on your tablet. Open Settings | Permissions required |
| Empty | No devices yet. Open Mullion on a tablet on the same Wi-Fi. | Get started by adding your first device! |

No exclamation marks, no emoji, no "seamless". Errors name the device and the fix.
