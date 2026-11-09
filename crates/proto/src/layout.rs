//! Layout map (PLAN "Layout map"): screens on a grid around the host.
//!
//! The host's main display sits in cell (0, 0) and never moves. Every other
//! screen owns one cell `(col, row)`; col grows to the right, row grows down
//! (the OS convention). The grid is what people drag on; [`origins`] turns it
//! into real display origins that touch and never overlap, and
//! [`infer_cells`] reads a grid back from wherever the OS has the displays
//! now (the user may have moved them in System Settings).
//!
//! Pure geometry, no OS calls. All coordinates are in the OS's display space
//! with the host at (0, 0).

use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};

/// A grid cell: `(col, row)`.
pub type Cell = (i32, i32);

/// How far from the host a screen may sit, in cells.
pub const MAX_REACH: i32 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }
    fn l(&self) -> i64 {
        self.x as i64
    }
    fn t(&self) -> i64 {
        self.y as i64
    }
    fn r(&self) -> i64 {
        self.x as i64 + self.w as i64
    }
    fn b(&self) -> i64 {
        self.y as i64 + self.h as i64
    }
    /// Interiors intersect (touching edges do not count).
    pub fn overlaps(&self, o: &Rect) -> bool {
        self.l() < o.r() && o.l() < self.r() && self.t() < o.b() && o.t() < self.b()
    }
    /// Share a stretch of edge (not just a corner).
    pub fn touches(&self, o: &Rect) -> bool {
        let v = self.t() < o.b() && o.t() < self.b();
        let h = self.l() < o.r() && o.l() < self.r();
        (v && (self.r() == o.l() || o.r() == self.l())) || (h && (self.b() == o.t() || o.b() == self.t()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutError {
    /// Something tried to take the host's cell.
    HostCell,
    /// Two screens in one cell.
    Shared(Cell),
    /// Farther than [`MAX_REACH`] from the host.
    TooFar(Cell),
    /// Not linked to the host through neighbouring cells.
    Detached(Cell),
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LayoutError::HostCell => write!(f, "the middle cell belongs to the host"),
            LayoutError::Shared((c, r)) => write!(f, "two screens in cell ({c}, {r})"),
            LayoutError::TooFar((c, r)) => write!(f, "cell ({c}, {r}) is more than {MAX_REACH} cells from the host"),
            LayoutError::Detached((c, r)) => write!(f, "cell ({c}, {r}) does not touch the other screens"),
        }
    }
}

impl std::error::Error for LayoutError {}

fn neighbours8((c, r): Cell) -> impl Iterator<Item = Cell> {
    (-1..=1).flat_map(move |dc| (-1..=1).map(move |dr| (c + dc, r + dr))).filter(move |n| *n != (c, r))
}

/// Cells of the non-host screens must be distinct, off the host cell, within
/// reach, and linked to the host through 8-neighbours (corners count, so an
/// arrangement the OS already has is never rejected).
pub fn validate(cells: &[Cell]) -> Result<(), LayoutError> {
    let mut seen = HashSet::new();
    for &c in cells {
        if c == (0, 0) {
            return Err(LayoutError::HostCell);
        }
        if c.0.abs() > MAX_REACH || c.1.abs() > MAX_REACH {
            return Err(LayoutError::TooFar(c));
        }
        if !seen.insert(c) {
            return Err(LayoutError::Shared(c));
        }
    }
    let mut reached: HashSet<Cell> = HashSet::from([(0, 0)]);
    let mut queue = VecDeque::from([(0, 0)]);
    while let Some(c) = queue.pop_front() {
        for n in neighbours8(c) {
            if seen.contains(&n) && reached.insert(n) {
                queue.push_back(n);
            }
        }
    }
    match cells.iter().find(|c| !reached.contains(c)) {
        Some(&c) => Err(LayoutError::Detached(c)),
        None => Ok(()),
    }
}

/// Order screens are placed in: nearest the host first, the host's own row
/// and column before diagonals.
fn placement_order(cells: &[Cell]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..cells.len()).collect();
    idx.sort_by_key(|&i| {
        let (c, r) = cells[i];
        (c.abs() + r.abs(), r.abs(), c.abs(), c, r)
    });
    idx
}

/// Display origins for screens of `sizes` (w, h) at `cells`, around a host of
/// `host` (w, h) at (0, 0).
///
/// 1. Columns and rows become tracks as wide/tall as their largest screen,
///    with the host's track centred on the host.
/// 2. Each screen sits in its track aligned towards the host (left-aligned
///    right of it, right-aligned left of it, centred in the host's own column
///    or row).
/// 3. Nearest first, each screen slides towards the host until it meets a
///    screen already placed, so neighbours touch instead of leaving a gap where
///    a track was wider than they are.
///
/// Tracks are disjoint and a slide stops at the first screen in the way, so
/// the result never overlaps.
pub fn origins(host: (u32, u32), cells: &[Cell], sizes: &[(u32, u32)]) -> Vec<(i32, i32)> {
    assert_eq!(cells.len(), sizes.len());
    let (hw, hh) = (host.0 as i64, host.1 as i64);
    let mut col_w = std::collections::BTreeMap::<i32, i64>::from([(0, hw)]);
    let mut row_h = std::collections::BTreeMap::<i32, i64>::from([(0, hh)]);
    for (&(c, r), &(w, h)) in cells.iter().zip(sizes) {
        let e = col_w.entry(c).or_insert(0);
        *e = (*e).max(w as i64);
        let e = row_h.entry(r).or_insert(0);
        *e = (*e).max(h as i64);
    }
    // Track spans [start, end) along one axis; track 0 is centred on the host.
    fn span(tracks: &std::collections::BTreeMap<i32, i64>, host_len: i64, k: i32) -> (i64, i64) {
        let len = |i: i32| *tracks.get(&i).unwrap_or(&0);
        let start0 = (host_len - len(0)) / 2;
        let end0 = start0 + len(0);
        if k == 0 {
            (start0, end0)
        } else if k > 0 {
            let s = end0 + (1..k).map(len).sum::<i64>();
            (s, s + len(k))
        } else {
            let e = start0 - (k + 1..0).map(len).sum::<i64>();
            (e - len(k), e)
        }
    }
    let track_pos = |k: i32, size: i64, tracks: &std::collections::BTreeMap<i32, i64>, host_len: i64| -> i64 {
        let (s, e) = span(tracks, host_len, k);
        match k.signum() {
            1 => s,
            -1 => e - size,
            _ => (host_len - size) / 2,
        }
    };

    let occupied: HashSet<Cell> = cells.iter().copied().chain(std::iter::once((0, 0))).collect();
    let mut out = vec![(0i32, 0i32); cells.len()];
    let mut placed: Vec<Rect> = vec![Rect::new(0, 0, host.0, host.1)];
    for i in placement_order(cells) {
        let (c, r) = cells[i];
        let (w, h) = (sizes[i].0 as i64, sizes[i].1 as i64);
        let mut x = track_pos(c, w, &col_w, hw);
        let mut y = track_pos(r, h, &row_h, hh);
        // Slide only towards the grid neighbours the screen has, so it stays in
        // its own cell; with none on either side it closes the gap both ways.
        let toward_h = c != 0 && occupied.contains(&(c - c.signum(), r));
        let toward_v = r != 0 && occupied.contains(&(c, r - r.signum()));
        let any_neighbour = [(c + 1, r), (c - 1, r), (c, r + 1), (c, r - 1)].iter().any(|n| occupied.contains(n));
        let (slide_h, slide_v) = if toward_h || toward_v {
            (toward_h, toward_v)
        } else if any_neighbour {
            // Its only neighbours are farther out; they slide to meet it.
            (false, false)
        } else {
            (c != 0, r != 0)
        };
        // Slide horizontally towards the host column.
        if slide_h {
            let band = |p: &Rect| p.t() < y + h && y < p.b();
            if c > 0 {
                if let Some(stop) = placed.iter().filter(|p| band(p) && p.r() <= x).map(|p| p.r()).max() {
                    x = stop;
                }
            } else if let Some(stop) = placed.iter().filter(|p| band(p) && p.l() >= x + w).map(|p| p.l()).min() {
                x = stop - w;
            }
        }
        // Then vertically towards the host row.
        if slide_v {
            let band = |p: &Rect| p.l() < x + w && x < p.r();
            if r > 0 {
                if let Some(stop) = placed.iter().filter(|p| band(p) && p.b() <= y).map(|p| p.b()).max() {
                    y = stop;
                }
            } else if let Some(stop) = placed.iter().filter(|p| band(p) && p.t() >= y + h).map(|p| p.t()).min() {
                y = stop - h;
            }
        }
        out[i] = (x as i32, y as i32);
        placed.push(Rect::new(x as i32, y as i32, w as u32, h as u32));
    }
    out
}

/// Ranks along one axis from a sweep outwards: a new rank whenever a screen
/// starts past the end of the previous one, stacked screens sharing a rank.
/// Sees steps that have nothing directly between them and the host, but a
/// wide screen anywhere hides the steps behind it.
fn walk_ranks(host: (i64, i64), spans: &[(i64, i64)]) -> Vec<i32> {
    let (hs, he) = host;
    let mut rank = vec![0i32; spans.len()];
    let tol = |i: usize| (spans[i].1 - spans[i].0) / 4;
    let mut fwd: Vec<usize> = (0..spans.len()).filter(|&i| spans[i].0 >= he - tol(i)).collect();
    fwd.sort_by_key(|&i| spans[i].0);
    let (mut k, mut edge) = (0, he);
    for i in fwd {
        if spans[i].0 >= edge - tol(i) {
            k += 1;
            edge = spans[i].1;
        } else {
            edge = edge.max(spans[i].1);
        }
        rank[i] = k;
    }
    let mut back: Vec<usize> = (0..spans.len()).filter(|&i| rank[i] == 0 && spans[i].1 <= hs + tol(i)).collect();
    back.sort_by_key(|&i| std::cmp::Reverse(spans[i].1));
    let (mut k, mut edge) = (0, hs);
    for i in back {
        if spans[i].1 <= edge + tol(i) {
            k -= 1;
            edge = spans[i].0;
        } else {
            edge = edge.min(spans[i].0);
        }
        rank[i] = k;
    }
    rank
}

/// Each method only ever undercounts, in different situations; take the
/// larger step count of the two.
fn axis_ranks(host: (i64, i64), main: &[(i64, i64)], cross: &[(i64, i64)]) -> Vec<i32> {
    let a = walk_ranks(host, main);
    let b = chain_ranks(host, main, cross);
    a.iter().zip(&b).map(|(&x, &y)| if x.abs() >= y.abs() { x } else { y }).collect()
}

/// Ranks along one axis. A screen past the host's far edge gets one more than
/// the highest-ranked screen directly between it and the host (overlapping it
/// on the other axis and ending where it starts), or 1 when nothing is in
/// between; mirrored for the near side. Screens level with the host are 0.
/// Counting along these chains, not across the whole desk, keeps a tall screen
/// in one column from hiding the steps in another.
fn chain_ranks(host: (i64, i64), main: &[(i64, i64)], cross: &[(i64, i64)]) -> Vec<i32> {
    let (hs, he) = host;
    let n = main.len();
    let tol = |i: usize| (main[i].1 - main[i].0) / 4;
    let fwd = |i: usize| main[i].0 >= he - tol(i);
    let back = |i: usize| !fwd(i) && main[i].1 <= hs + tol(i);
    let cross_overlap = |a: (i64, i64), b: (i64, i64)| a.0 < b.1 && b.0 < a.1;
    let mut rank = vec![0i32; n];
    // Forward: nearest first, so everything in between is ranked already.
    let mut order: Vec<usize> = (0..n).filter(|&i| fwd(i)).collect();
    order.sort_by_key(|&i| main[i].0);
    for &i in &order {
        let behind = (0..n)
            .filter(|&j| j != i && !back(j) && (rank[j] > 0 || !fwd(j)))
            .filter(|&j| cross_overlap(cross[i], cross[j]) && main[j].1 <= main[i].0 + tol(i))
            .map(|j| rank[j])
            .max();
        rank[i] = 1 + behind.unwrap_or(0);
    }
    let mut order: Vec<usize> = (0..n).filter(|&i| back(i)).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(main[i].1));
    for &i in &order {
        let behind = (0..n)
            .filter(|&j| j != i && !fwd(j) && (rank[j] < 0 || !back(j)))
            .filter(|&j| cross_overlap(cross[i], cross[j]) && main[j].0 >= main[i].1 - tol(i))
            .map(|j| rank[j])
            .min();
        rank[i] = -1 + behind.unwrap_or(0);
    }
    rank
}

/// The grid cell of each screen in `others`, read from where the OS has it
/// relative to `host`. Collisions (and anything landing on the host cell) move
/// to the nearest free cell, so the result always passes the uniqueness rules.
pub fn infer_cells(host: Rect, others: &[Rect]) -> Vec<Cell> {
    let xs: Vec<(i64, i64)> = others.iter().map(|o| (o.l(), o.r())).collect();
    let ys: Vec<(i64, i64)> = others.iter().map(|o| (o.t(), o.b())).collect();
    let cols = axis_ranks((host.l(), host.r()), &xs, &ys);
    let rows = axis_ranks((host.t(), host.b()), &ys, &xs);
    let mut taken: HashSet<Cell> = HashSet::from([(0, 0)]);
    // Nearest first, so the closest screen keeps a contested cell.
    let mut order: Vec<usize> = (0..others.len()).collect();
    order.sort_by_key(|&i| (cols[i].abs() + rows[i].abs(), i));
    let mut cells = vec![(0, 0); others.len()];
    for i in order {
        let want = (cols[i].clamp(-MAX_REACH, MAX_REACH), rows[i].clamp(-MAX_REACH, MAX_REACH));
        let got = if taken.contains(&want) { nearest_free(want, &taken) } else { want };
        taken.insert(got);
        cells[i] = got;
    }
    cells
}

/// Closest free cell to `want` (ring by ring, ties broken by distance then
/// position), never the host cell.
pub fn nearest_free(want: Cell, taken: &HashSet<Cell>) -> Cell {
    for ring in 1..=(2 * MAX_REACH) {
        let mut ring_cells: Vec<Cell> = (-ring..=ring)
            .flat_map(|dc| (-ring..=ring).map(move |dr| (dc, dr)))
            .filter(|(dc, dr)| dc.abs().max(dr.abs()) == ring)
            .map(|(dc, dr)| (want.0 + dc, want.1 + dr))
            .filter(|c| !taken.contains(c) && c.0.abs() <= MAX_REACH && c.1.abs() <= MAX_REACH)
            .collect();
        ring_cells.sort_by_key(|c| ((c.0 - want.0).pow(2) + (c.1 - want.1).pow(2), c.0, c.1));
        if let Some(c) = ring_cells.first() {
            return *c;
        }
    }
    want
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: (u32, u32) = (1512, 982);
    const TAB: (u32, u32) = (1920, 1080);

    fn rects(cells: &[Cell], sizes: &[(u32, u32)], host: (u32, u32)) -> Vec<Rect> {
        origins(host, cells, sizes).iter().zip(sizes).map(|(&(x, y), &(w, h))| Rect::new(x, y, w, h)).collect()
    }

    fn assert_sane(host: (u32, u32), cells: &[Cell], sizes: &[(u32, u32)]) -> Vec<Rect> {
        let rs = rects(cells, sizes, host);
        let all: Vec<Rect> = std::iter::once(Rect::new(0, 0, host.0, host.1)).chain(rs.iter().copied()).collect();
        for i in 0..all.len() {
            for j in i + 1..all.len() {
                assert!(!all[i].overlaps(&all[j]), "overlap {:?} {:?} for {cells:?}", all[i], all[j]);
            }
        }
        // Everything reaches the host through shared edges.
        let mut reached = vec![false; all.len()];
        reached[0] = true;
        let mut changed = true;
        while changed {
            changed = false;
            for i in 0..all.len() {
                if reached[i] {
                    continue;
                }
                if (0..all.len()).any(|j| reached[j] && all[i].touches(&all[j])) {
                    reached[i] = true;
                    changed = true;
                }
            }
        }
        assert!(reached.iter().all(|r| *r), "detached screen in {cells:?}: {all:?}");
        rs
    }

    #[test]
    fn each_side_alone_touches_the_host_centred() {
        let one = |c: Cell| assert_sane(MAC, &[c], &[TAB])[0];
        let r = one((1, 0));
        assert_eq!((r.x, r.y), (1512, (982 - 1080) / 2), "right, vertically centred");
        let r = one((-1, 0));
        assert_eq!((r.x, r.y), (-1920, (982 - 1080) / 2), "left");
        let r = one((0, -1));
        assert_eq!((r.x, r.y), ((1512 - 1920) / 2, -1080), "above, horizontally centred");
        let r = one((0, 1));
        assert_eq!((r.x, r.y), ((1512 - 1920) / 2, 982), "below");
    }

    #[test]
    fn all_four_sides_at_once_stay_apart() {
        // The taller side screens reach past the host's top, so the one above
        // rests on them rather than overlapping; it still sits above the host.
        let r = assert_sane(MAC, &[(1, 0), (-1, 0), (0, -1), (0, 1)], &[TAB; 4]);
        assert!(r[2].y + 1080 <= 0 && r[3].y >= 982);
    }

    #[test]
    fn a_row_of_screens_chains_edge_to_edge() {
        let r = assert_sane(MAC, &[(1, 0), (2, 0), (3, 0)], &[TAB, (1280, 800), TAB]);
        assert_eq!(r[0].x, 1512);
        assert_eq!(r[1].x, 1512 + 1920);
        assert_eq!(r[2].x, 1512 + 1920 + 1280);
    }

    #[test]
    fn a_gap_in_the_grid_closes_up() {
        // Column 1 empty: the screen in column 2 slides against the host.
        let r = assert_sane(MAC, &[(2, 0)], &[TAB]);
        assert_eq!(r[0].x, 1512);
    }

    #[test]
    fn diagonal_sits_under_its_neighbour() {
        let r = assert_sane(MAC, &[(1, 0), (1, 1)], &[TAB, TAB]);
        assert_eq!(r[1].x, r[0].x);
        assert_eq!(r[1].y, r[0].y + 1080);
    }

    #[test]
    fn mixed_sizes_never_overlap_and_stay_linked() {
        // Deterministic pseudo-random layouts grown from the host.
        let sizes_pool = [TAB, (1280, 800), (2560, 1440), (1024, 1366), (800, 1280), MAC];
        let mut seed: u64 = 0x9E3779B97F4A7C15;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..400 {
            let n = 1 + (next() % 6) as usize;
            let mut cells: Vec<Cell> = vec![];
            let mut taken: HashSet<Cell> = HashSet::from([(0, 0)]);
            while cells.len() < n {
                // Grow from a random occupied cell to a 4-neighbour.
                let base = if cells.is_empty() || next() % 3 == 0 { (0, 0) } else { cells[(next() as usize) % cells.len()] };
                let d = [(1, 0), (-1, 0), (0, 1), (0, -1)][(next() % 4) as usize];
                let c = (base.0 + d.0, base.1 + d.1);
                if taken.insert(c) {
                    cells.push(c);
                }
            }
            let sizes: Vec<(u32, u32)> = (0..n).map(|_| sizes_pool[(next() % sizes_pool.len() as u64) as usize]).collect();
            let host = sizes_pool[(next() % sizes_pool.len() as u64) as usize];
            assert!(validate(&cells).is_ok());
            assert_sane(host, &cells, &sizes);
        }
    }

    #[test]
    fn reading_back_a_layout_gives_the_same_grid() {
        let cases: Vec<(Vec<Cell>, Vec<(u32, u32)>)> = vec![
            (vec![(1, 0)], vec![TAB]),
            (vec![(-1, 0)], vec![TAB]),
            (vec![(0, -1)], vec![TAB]),
            (vec![(0, 1)], vec![(1280, 800)]),
            (vec![(1, 0), (2, 0), (3, 0), (4, 0), (5, 0), (6, 0)], vec![TAB; 6]),
            (vec![(1, 0), (-1, 0), (0, 1), (0, -1)], vec![TAB, (1280, 800), (2560, 1440), TAB]),
            (vec![(1, 0), (1, 1), (0, 1)], vec![TAB, TAB, TAB]),
            (vec![(1, 0), (1, -1), (-1, 0), (-1, 1)], vec![(1280, 800), TAB, TAB, (1024, 1366)]),
        ];
        for (cells, sizes) in cases {
            let rs = rects(&cells, &sizes, MAC);
            let back = infer_cells(Rect::new(0, 0, MAC.0, MAC.1), &rs);
            assert_eq!(back, cells, "round trip of {rs:?}");
        }
    }

    #[test]
    fn reads_the_line_up_the_host_makes_today() {
        // arrange.rs LineUp: virtual displays in a row right of the built-in, top-aligned.
        let rs = [Rect::new(1512, 0, 1920, 1080), Rect::new(3432, 0, 1920, 1080)];
        assert_eq!(infer_cells(Rect::new(0, 0, 1512, 982), &rs), vec![(1, 0), (2, 0)]);
    }

    #[test]
    fn collisions_move_to_the_nearest_free_cell() {
        // Two screens stacked exactly on top of each other right of the host.
        let rs = [Rect::new(1512, 0, 1920, 1080), Rect::new(1512, 0, 1920, 1080)];
        let cells = infer_cells(Rect::new(0, 0, 1512, 982), &rs);
        assert_eq!(cells[0], (1, 0));
        assert_ne!(cells[1], (1, 0));
        assert!(validate(&cells).is_ok(), "{cells:?}");
    }

    #[test]
    fn validate_names_the_problem() {
        assert_eq!(validate(&[(0, 0)]), Err(LayoutError::HostCell));
        assert_eq!(validate(&[(1, 0), (1, 0)]), Err(LayoutError::Shared((1, 0))));
        assert_eq!(validate(&[(3, 0)]), Err(LayoutError::Detached((3, 0))));
        assert_eq!(validate(&[(MAX_REACH + 1, 0)]), Err(LayoutError::TooFar((MAX_REACH + 1, 0))));
        assert!(validate(&[(1, 1)]).is_ok(), "corners link");
        assert!(validate(&[]).is_ok());
    }
}

#[cfg(test)]
mod roundtrip {
    use super::*;

    /// Random layouts grown from the host, placed by [`origins`] and read back
    /// by [`infer_cells`]: how many come back as the same grid.
    fn round_trip_rate(pool: &[(u32, u32)], seed: u64) -> f64 {
        let mut seed = seed;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut ok = 0;
        for _ in 0..1000 {
            let n = 1 + (next() % 5) as usize;
            let mut cells: Vec<Cell> = vec![];
            let mut taken: HashSet<Cell> = HashSet::from([(0, 0)]);
            while cells.len() < n {
                let base = if cells.is_empty() || next() % 3 == 0 { (0, 0) } else { cells[(next() as usize) % cells.len()] };
                let d = [(1, 0), (-1, 0), (0, 1), (0, -1)][(next() % 4) as usize];
                let c = (base.0 + d.0, base.1 + d.1);
                if taken.insert(c) {
                    cells.push(c);
                }
            }
            let sizes: Vec<(u32, u32)> = (0..n).map(|_| pool[(next() % pool.len() as u64) as usize]).collect();
            let rs: Vec<Rect> = origins((1512, 982), &cells, &sizes).iter().zip(&sizes).map(|(&(x, y), &(w, h))| Rect::new(x, y, w, h)).collect();
            if infer_cells(Rect::new(0, 0, 1512, 982), &rs) == cells {
                ok += 1;
            }
        }
        ok as f64 / 1000.0
    }

    #[test]
    fn same_size_screens_always_read_back() {
        assert_eq!(round_trip_rate(&[(1920, 1080)], 0x1234567), 1.0);
        assert_eq!(round_trip_rate(&[(1920, 1080)], 0xC0FFEE), 1.0);
    }

    #[test]
    fn mixed_sizes_almost_always_read_back() {
        // Misses are stacks like a 2560-wide screen over a 1024-wide one, where
        // the geometry itself is ambiguous; the grid then shows the nearest cell.
        let pool = [(1920, 1080), (1280, 800), (2560, 1440), (1024, 1366), (1512, 982)];
        assert!(round_trip_rate(&pool, 0x1234567) >= 0.98);
        assert!(round_trip_rate(&pool, 0xC0FFEE) >= 0.98);
    }
}
