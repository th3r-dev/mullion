//! The layout map over HTTP (`GET /screens`, `POST /screens/layout`).
//!
//! Every display the OS shows the desktop on becomes a tile: the main display
//! is the host, fixed in grid cell (0, 0); a virtual display belongs to the
//! device that asked for it; anything else is one of the Mac's own displays.
//! Cells are read from where the OS has the displays now
//! ([`mullion_proto::layout::infer_cells`]), and a requested layout is turned
//! back into display origins with [`mullion_proto::layout::origins`].

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use mullion_proto::layout::{self, Cell, Rect};
use serde::{Deserialize, Serialize};

use crate::platform::{DisplayInfo, Platform};

/// Who owns a virtual display, published by the session loop.
#[derive(Debug, Clone)]
pub struct ScreenOwner {
    pub display_id: u32,
    /// Key the session loop files the display under (device id, or session-N).
    pub key: String,
    pub device_id: Option<String>,
    pub device_name: String,
    pub connected: bool,
    /// Set while the device is gone and its screen waits out the grace period.
    pub removes_in_secs: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ScreenKind {
    /// The main display, fixed in the middle of the map.
    Host,
    /// Another display of this computer (built-in or external monitor).
    Display,
    /// A virtual display a receiver device asked for (Extend mode).
    Device,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScreenView {
    pub display_id: u32,
    pub kind: ScreenKind,
    pub name: String,
    pub builtin: bool,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub col: i32,
    pub row: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connected: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removes_in_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ScreensView {
    pub host_name: String,
    /// False on an OS where the host cannot move displays yet.
    pub can_arrange: bool,
    pub max_reach: i32,
    pub screens: Vec<ScreenView>,
}

/// One requested cell, from `POST /screens/layout`.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub struct CellAssign {
    pub display_id: u32,
    pub col: i32,
    pub row: i32,
}

#[derive(Debug, Deserialize)]
pub struct LayoutRequest {
    pub cells: Vec<CellAssign>,
}

/// A requested layout that cannot be placed (409, not a server fault).
#[derive(Debug)]
pub struct Rejected(pub String);

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Rejected {}

fn split_host(displays: &[DisplayInfo]) -> Result<(DisplayInfo, Vec<DisplayInfo>)> {
    let host = displays
        .iter()
        .copied()
        .find(|d| d.main)
        .or_else(|| displays.iter().copied().find(|d| d.x == 0 && d.y == 0))
        .ok_or_else(|| anyhow!("no main display"))?;
    let others = displays.iter().copied().filter(|d| d.id != host.id).collect();
    Ok((host, others))
}

fn rect(d: &DisplayInfo, host: &DisplayInfo) -> Rect {
    // Relative to the host, which the OS normally already has at the origin.
    Rect::new(d.x - host.x, d.y - host.y, d.width, d.height)
}

/// The grid as the OS has it now: (display id, cell) for every non-host display.
pub fn current_cells(displays: &[DisplayInfo]) -> Result<(DisplayInfo, Vec<(DisplayInfo, Cell)>)> {
    let (host, others) = split_host(displays)?;
    let cells = layout::infer_cells(rect(&host, &host), &others.iter().map(|d| rect(d, &host)).collect::<Vec<_>>());
    Ok((host, others.into_iter().zip(cells).collect()))
}

/// Origins that realise `requested` (other displays keep their cells), and the
/// final cell of every display. Pure, so it is tested without an OS.
pub fn plan(displays: &[DisplayInfo], requested: &[CellAssign]) -> Result<(Vec<(u32, i32, i32)>, Vec<(u32, Cell)>)> {
    let (host, current) = current_cells(displays)?;
    let mut cells: HashMap<u32, Cell> = current.iter().map(|(d, c)| (d.id, *c)).collect();
    for r in requested {
        if r.display_id == host.id {
            if (r.col, r.row) != (0, 0) {
                return Err(Rejected(format!("{} is the host and stays in the middle", r.display_id)).into());
            }
            continue;
        }
        match cells.get_mut(&r.display_id) {
            Some(c) => *c = (r.col, r.row),
            None => return Err(Rejected(format!("display {} is not connected any more", r.display_id)).into()),
        }
    }
    let order: Vec<&DisplayInfo> = current.iter().map(|(d, _)| d).collect();
    let grid: Vec<Cell> = order.iter().map(|d| cells[&d.id]).collect();
    layout::validate(&grid).map_err(|e| Rejected(format!("Can't place the screens there: {e}.")))?;
    let sizes: Vec<(u32, u32)> = order.iter().map(|d| (d.width, d.height)).collect();
    let origins = layout::origins((host.width, host.height), &grid, &sizes);
    let moves = order
        .iter()
        .zip(&origins)
        .map(|(d, &(x, y))| (d.id, x + host.x, y + host.y))
        .collect();
    Ok((moves, order.iter().map(|d| d.id).zip(grid).collect()))
}

/// What `GET /screens` returns.
pub fn view(platform: &dyn Platform, owners: &[ScreenOwner]) -> Result<ScreensView> {
    let displays = platform.displays()?;
    if displays.is_empty() {
        return Ok(ScreensView { can_arrange: false, max_reach: layout::MAX_REACH, ..Default::default() });
    }
    let (host, current) = current_cells(&displays)?;
    let mut screens = vec![ScreenView {
        display_id: host.id,
        kind: ScreenKind::Host,
        name: String::new(), // the signaling layer fills in the host name
        builtin: host.builtin,
        x: host.x,
        y: host.y,
        width: host.width,
        height: host.height,
        col: 0,
        row: 0,
        device_id: None,
        connected: None,
        removes_in_secs: None,
    }];
    let mut n_display = 1;
    for (d, (col, row)) in current {
        let owner = owners.iter().find(|o| o.display_id == d.id);
        let (kind, name) = match owner {
            Some(o) => (ScreenKind::Device, o.device_name.clone()),
            None if d.builtin => (ScreenKind::Display, "Built-in display".to_string()),
            None => {
                n_display += 1;
                (ScreenKind::Display, format!("Display {n_display}"))
            }
        };
        screens.push(ScreenView {
            display_id: d.id,
            kind,
            name,
            builtin: d.builtin,
            x: d.x,
            y: d.y,
            width: d.width,
            height: d.height,
            col,
            row,
            device_id: owner.and_then(|o| o.device_id.clone()),
            connected: owner.map(|o| o.connected),
            removes_in_secs: owner.and_then(|o| o.removes_in_secs),
        });
    }
    Ok(ScreensView { host_name: String::new(), can_arrange: true, max_reach: layout::MAX_REACH, screens })
}

/// Apply a requested layout. Returns the cells of device-owned screens, keyed
/// by owner, so the session can put a device back there when it reconnects.
pub fn apply(platform: &dyn Platform, owners: &[ScreenOwner], requested: &[CellAssign]) -> Result<Vec<(String, Cell)>> {
    let displays = platform.displays()?;
    let (moves, cells) = plan(&displays, requested)?;
    platform.set_origins(&moves)?;
    Ok(cells
        .into_iter()
        .filter_map(|(id, c)| owners.iter().find(|o| o.display_id == id).map(|o| (o.key.clone(), c)))
        .collect())
}

/// Put one display into `cell` (a device coming back to where it was), if
/// that cell is still free and the result is a valid layout.
pub fn place(platform: &dyn Platform, display_id: u32, cell: Cell) -> Result<bool> {
    let displays = platform.displays()?;
    let (_, current) = current_cells(&displays)?;
    if current.iter().any(|(d, c)| *c == cell && d.id != display_id) {
        return Ok(false);
    }
    match plan(&displays, &[CellAssign { display_id, col: cell.0, row: cell.1 }]) {
        Ok((moves, _)) => {
            platform.set_origins(&moves)?;
            Ok(true)
        }
        Err(e) if e.is::<Rejected>() => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(id: u32, x: i32, y: i32, w: u32, h: u32, main: bool) -> DisplayInfo {
        DisplayInfo { id, x, y, width: w, height: h, main, builtin: main }
    }

    /// Built-in 1512x982 main, two tablets lined up to its right (what
    /// arrange.rs LineUp produces today).
    fn desk() -> Vec<DisplayInfo> {
        vec![d(1, 0, 0, 1512, 982, true), d(7, 1512, 0, 1920, 1080, false), d(9, 3432, 0, 1920, 1080, false)]
    }

    #[test]
    fn reads_the_current_grid() {
        let (host, cells) = current_cells(&desk()).unwrap();
        assert_eq!(host.id, 1);
        assert_eq!(cells.iter().map(|(d, c)| (d.id, *c)).collect::<Vec<_>>(), vec![(7, (1, 0)), (9, (2, 0))]);
    }

    #[test]
    fn moving_one_screen_keeps_the_others() {
        let (moves, cells) = plan(&desk(), &[CellAssign { display_id: 9, col: -1, row: 0 }]).unwrap();
        assert_eq!(cells, vec![(7, (1, 0)), (9, (-1, 0))]);
        assert_eq!(moves.iter().find(|m| m.0 == 9).unwrap().1, -1920, "left of the host");
        assert_eq!(moves.iter().find(|m| m.0 == 7).unwrap().1, 1512, "still right of the host");
    }

    #[test]
    fn swapping_two_screens() {
        let (_, cells) = plan(
            &desk(),
            &[CellAssign { display_id: 7, col: 2, row: 0 }, CellAssign { display_id: 9, col: 1, row: 0 }],
        )
        .unwrap();
        assert_eq!(cells, vec![(7, (2, 0)), (9, (1, 0))]);
    }

    #[test]
    fn rejects_the_host_cell_a_shared_cell_and_unknown_displays() {
        let rejected = |r: &[CellAssign]| plan(&desk(), r).unwrap_err().is::<Rejected>();
        assert!(rejected(&[CellAssign { display_id: 7, col: 0, row: 0 }]));
        assert!(rejected(&[CellAssign { display_id: 7, col: 2, row: 0 }]), "9 is already there");
        assert!(rejected(&[CellAssign { display_id: 1, col: 1, row: 0 }]), "the host stays put");
        assert!(rejected(&[CellAssign { display_id: 42, col: 0, row: 1 }]));
        assert!(rejected(&[CellAssign { display_id: 9, col: 4, row: 0 }]), "detached");
    }

    #[test]
    fn host_off_the_origin_still_works() {
        // Defensive: if the OS ever reports the main display elsewhere, cells are relative to it.
        let shifted: Vec<DisplayInfo> = desk().into_iter().map(|mut x| { x.x += 100; x.y += 50; x }).collect();
        let (moves, _) = plan(&shifted, &[CellAssign { display_id: 9, col: 0, row: 1 }]).unwrap();
        let m9 = moves.iter().find(|m| m.0 == 9).unwrap();
        // Below the host, centred; it rests on the taller tablet beside the host.
        assert_eq!((m9.1, m9.2), (100 + (1512 - 1920) / 2, 50 + (982 - 1080) / 2 + 1080));
    }
}
