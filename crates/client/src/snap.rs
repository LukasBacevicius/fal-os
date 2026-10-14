//! Snapping and guides while dragging, resizing and nudging (docs/research/paper-0515.md §1).
//!
//! - A moved box snaps its left / centre / right (top / middle / bottom) to the same lines of
//!   other nodes within [`SNAP_PX`] screen pixels; equal spacing to neighbours in a row or
//!   column snaps within 1.5× that.
//! - A resized box snaps only the edges that move.
//! - Every snap leaves 1 px guides (alignment lines, and the equal gaps with end ticks) that the
//!   overlay draws in the theme's dim gray.
//!
//! Pure geometry in world units: callers pass the threshold already divided by the zoom.

/// Snap distance in CSS pixels (divided by the zoom for world units).
pub const SNAP_PX: f32 = 4.0;
/// Equal-spacing snaps reach this much further than edge snaps.
pub const SPACING_FACTOR: f32 = 1.5;
/// How long guides stay after a keyboard nudge (ms).
pub const NUDGE_GUIDES_MS: f64 = 850.0;
/// Values closer than this (world units) count as aligned when drawing guides.
const EPS: f32 = 0.01;

/// A guide segment in world space. `ticks`: a measured gap (short end marks).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Line {
    pub a: [f32; 2],
    pub b: [f32; 2],
    pub ticks: bool,
}

/// Which edges of a box move (resize); a move moves all of them, centres included.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Edges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

impl Edges {
    pub const ALL: Edges = Edges { left: true, right: true, top: true, bottom: true };

    /// The edges a resize handle (0 TL, 1 T, 2 TR, 3 R, 4 BR, 5 B, 6 BL, 7 L) moves.
    pub fn of_handle(h: usize) -> Edges {
        Edges { left: matches!(h, 0 | 6 | 7), right: matches!(h, 2..=4), top: matches!(h, 0..=2), bottom: matches!(h, 4..=6) }
    }

    fn moving(&self, axis: usize) -> (bool, bool) {
        if axis == 0 { (self.left, self.right) } else { (self.top, self.bottom) }
    }

    fn is_move(&self) -> bool {
        *self == Edges::ALL
    }
}

fn lo(r: [f32; 4], axis: usize) -> f32 {
    r[axis]
}
fn size(r: [f32; 4], axis: usize) -> f32 {
    r[axis + 2]
}
fn hi(r: [f32; 4], axis: usize) -> f32 {
    r[axis] + r[axis + 2]
}
fn mid(r: [f32; 4], axis: usize) -> f32 {
    r[axis] + r[axis + 2] * 0.5
}
/// lo / mid / hi of `r` on `axis`.
fn stops(r: [f32; 4], axis: usize) -> [f32; 3] {
    [lo(r, axis), mid(r, axis), hi(r, axis)]
}

/// A point with `main` on `axis` and `cross` on the other one.
fn pt(axis: usize, main: f32, cross: f32) -> [f32; 2] {
    if axis == 0 { [main, cross] } else { [cross, main] }
}

/// Whether `a` and `b` overlap on `axis` (touching counts).
fn overlaps(a: [f32; 4], b: [f32; 4], axis: usize) -> bool {
    lo(a, axis) <= hi(b, axis) && lo(b, axis) <= hi(a, axis)
}

/// The values of `r` on `axis` that snap: all three for a move, else the moving edges.
fn snapping_values(r: [f32; 4], axis: usize, edges: Edges) -> Vec<f32> {
    if edges.is_move() {
        return stops(r, axis).to_vec();
    }
    let (l, h) = edges.moving(axis);
    let mut v = Vec::new();
    if l {
        v.push(lo(r, axis));
    }
    if h {
        v.push(hi(r, axis));
    }
    v
}

/// Closest alignment of `values` to any candidate's lo / mid / hi: `(offset, distance)`.
fn best_align(values: &[f32], axis: usize, cands: &[[f32; 4]], thr: f32) -> Option<(f32, f32)> {
    let mut best: Option<(f32, f32)> = None;
    for c in cands {
        for t in stops(*c, axis) {
            for v in values {
                let off = t - v;
                let d = off.abs();
                if d <= thr && best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((off, d));
                }
            }
        }
    }
    best
}

/// Equal-spacing positions for `r` on `axis` among the candidates in its row (column): the
/// new `lo` and the gap guides at that position.
fn spacing_options(r: [f32; 4], axis: usize, cands: &[[f32; 4]]) -> Vec<(f32, Vec<Line>)> {
    let cross = 1 - axis;
    let mut row: Vec<[f32; 4]> = cands.iter().copied().filter(|c| overlaps(*c, r, cross) && !overlaps(*c, r, axis)).collect();
    if row.is_empty() {
        return Vec::new();
    }
    row.sort_by(|a, b| lo(*a, axis).total_cmp(&lo(*b, axis)));
    let s = size(r, axis);
    // Gap guides sit in the middle of the two boxes' common cross range.
    let gap = |a: [f32; 4], b: [f32; 4], from: f32, to: f32| {
        let c0 = lo(a, cross).max(lo(b, cross));
        let c1 = hi(a, cross).min(hi(b, cross));
        let c = if c1 >= c0 { (c0 + c1) * 0.5 } else { mid(a, cross) };
        Line { a: pt(axis, from, c), b: pt(axis, to, c), ticks: true }
    };
    let at = |l: f32| {
        let mut m = r;
        m[axis] = l;
        m
    };
    let mut out = Vec::new();
    // Neighbours left and right of the box: the same gap on both sides (centred between them).
    let centre = mid(r, axis);
    let left = row.iter().copied().filter(|c| hi(*c, axis) <= centre).max_by(|a, b| hi(*a, axis).total_cmp(&hi(*b, axis)));
    let right = row.iter().copied().filter(|c| lo(*c, axis) >= centre).min_by(|a, b| lo(*a, axis).total_cmp(&lo(*b, axis)));
    if let (Some(a), Some(b)) = (left, right) {
        let free = lo(b, axis) - hi(a, axis) - s;
        if free >= 0.0 {
            let l = hi(a, axis) + free * 0.5;
            let m = at(l);
            out.push((l, vec![gap(a, m, hi(a, axis), l), gap(m, b, l + s, lo(b, axis))]));
        }
    }
    // Continue a run: after the last of two neighbours, or before the first.
    for w in row.windows(2) {
        let (a, b) = (w[0], w[1]);
        let g = lo(b, axis) - hi(a, axis);
        if g < 0.0 {
            continue;
        }
        if hi(b, axis) <= centre {
            let l = hi(b, axis) + g;
            let m = at(l);
            out.push((l, vec![gap(a, b, hi(a, axis), lo(b, axis)), gap(b, m, hi(b, axis), l)]));
        }
        if lo(a, axis) >= centre {
            let l = lo(a, axis) - g - s;
            let m = at(l);
            out.push((l, vec![gap(m, a, l + s, lo(a, axis)), gap(a, b, hi(a, axis), lo(b, axis))]));
        }
    }
    out
}

/// Snap result: the offset to add on each axis and the guides at the snapped position.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snap {
    pub offset: [f32; 2],
    pub guides: Vec<Line>,
}

/// Snaps a box whose `edges` move (all of them for a drag) against `cands` (world rects of
/// other nodes) within `thr` world units. Returns the offset per axis (applied to the moving
/// edges) and the guides for the result.
pub fn snap(r: [f32; 4], edges: Edges, cands: &[[f32; 4]], thr: f32) -> Snap {
    let mut offset = [0.0f32; 2];
    for (axis, o) in offset.iter_mut().enumerate() {
        let values = snapping_values(r, axis, edges);
        if values.is_empty() {
            continue;
        }
        // Compare in units of each kind's own threshold: spacing reaches 1.5× as far.
        let mut best = best_align(&values, axis, cands, thr).map(|(off, d)| (off, d / thr));
        if edges.is_move() {
            let sthr = thr * SPACING_FACTOR;
            for (l, _) in spacing_options(r, axis, cands) {
                let off = l - lo(r, axis);
                let d = off.abs();
                if d <= sthr && best.is_none_or(|(_, bd)| d / sthr < bd) {
                    best = Some((off, d / sthr));
                }
            }
        }
        if let Some((off, _)) = best {
            *o = off;
        }
    }
    let snapped = apply(r, edges, offset);
    Snap { offset, guides: guides(snapped, edges, cands) }
}

/// `r` with the moving edges shifted by `off` (a move shifts the whole box).
pub fn apply(r: [f32; 4], edges: Edges, off: [f32; 2]) -> [f32; 4] {
    let mut out = r;
    for axis in 0..2 {
        let (l, h) = edges.moving(axis);
        match (l, h) {
            (true, true) => out[axis] += off[axis],
            (true, false) => {
                out[axis] += off[axis];
                out[axis + 2] -= off[axis];
            }
            (false, true) => out[axis + 2] += off[axis],
            (false, false) => {}
        }
    }
    out
}

/// Guides for `r` as it is: every line it shares with a candidate (one line per value,
/// spanning all of them), plus equal gaps for a move.
pub fn guides(r: [f32; 4], edges: Edges, cands: &[[f32; 4]]) -> Vec<Line> {
    let mut out = Vec::new();
    for axis in 0..2 {
        let cross = 1 - axis;
        for v in snapping_values(r, axis, edges) {
            let mut span: Option<(f32, f32)> = None;
            for c in cands {
                if stops(*c, axis).iter().any(|t| (t - v).abs() <= EPS) {
                    let (a, b) = (lo(*c, cross).min(lo(r, cross)), hi(*c, cross).max(hi(r, cross)));
                    span = Some(span.map_or((a, b), |(x, y)| (x.min(a), y.max(b))));
                }
            }
            if let Some((a, b)) = span {
                out.push(Line { a: pt(axis, v, a), b: pt(axis, v, b), ticks: false });
            }
        }
        if edges.is_move() {
            for (l, lines) in spacing_options(r, axis, cands) {
                if (l - lo(r, axis)).abs() <= EPS {
                    out.extend(lines);
                    break;
                }
            }
        }
    }
    out
}

/// ⌥ measurements between the selection bounds `a` and a hovered node `b`: gap lines with
/// their lengths (world units), or the insets when one contains the other.
pub fn measure(a: [f32; 4], b: [f32; 4]) -> Vec<(Line, f32)> {
    let mut out = Vec::new();
    let contains = |o: [f32; 4], i: [f32; 4]| lo(o, 0) <= lo(i, 0) && lo(o, 1) <= lo(i, 1) && hi(o, 0) >= hi(i, 0) && hi(o, 1) >= hi(i, 1);
    let (inner, outer) = if contains(b, a) {
        (Some(a), b)
    } else if contains(a, b) {
        (Some(b), a)
    } else {
        (None, b)
    };
    if let Some(i) = inner {
        // Insets on all four sides, through the inner box's centre.
        for axis in 0..2 {
            let c = mid(i, 1 - axis);
            for (from, to) in [(lo(outer, axis), lo(i, axis)), (hi(i, axis), hi(outer, axis))] {
                if to - from > EPS {
                    out.push((Line { a: pt(axis, from, c), b: pt(axis, to, c), ticks: true }, to - from));
                }
            }
        }
        return out;
    }
    for axis in 0..2 {
        let cross = 1 - axis;
        let (from, to) = if hi(a, axis) <= lo(b, axis) {
            (hi(a, axis), lo(b, axis))
        } else if hi(b, axis) <= lo(a, axis) {
            (hi(b, axis), lo(a, axis))
        } else {
            continue;
        };
        // In the middle of the shared cross range, else through the selection's centre.
        let c0 = lo(a, cross).max(lo(b, cross));
        let c1 = hi(a, cross).min(hi(b, cross));
        let c = if c1 >= c0 { (c0 + c1) * 0.5 } else { mid(a, cross) };
        out.push((Line { a: pt(axis, from, c), b: pt(axis, to, c), ticks: true }, to - from));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: [f32; 4] = [0.0, 0.0, 100.0, 100.0];
    const B: [f32; 4] = [200.0, 0.0, 100.0, 100.0];

    #[test]
    fn snaps_edges_within_the_threshold_only() {
        // Left edge 2 away from B's right edge (300): snaps.
        let s = snap([302.0, 150.0, 50.0, 50.0], Edges::ALL, &[A, B], 4.0);
        assert_eq!(s.offset[0], -2.0);
        assert!(s.guides.iter().any(|g| g.a[0] == 300.0 && g.b[0] == 300.0 && !g.ticks));
        // 5 away: no snap on x.
        let s = snap([305.0, 150.0, 50.0, 50.0], Edges::ALL, &[A, B], 4.0);
        assert_eq!(s.offset[0], 0.0);
    }

    #[test]
    fn snaps_centres_and_picks_the_closest() {
        // Centre 49 is 1 away from A's centre (50); nothing else is within 4.
        let s = snap([24.0, 300.0, 50.0, 50.0], Edges::ALL, &[A], 4.0);
        assert_eq!(s.offset[0], 1.0);
        let g = s.guides.iter().find(|g| g.a[0] == 50.0).expect("centre guide");
        assert_eq!((g.a[1], g.b[1]), (0.0, 350.0));
    }

    #[test]
    fn threshold_scales_with_zoom() {
        // At 4× zoom, 4 screen px is 1 world unit: 2 away no longer snaps.
        let thr = SNAP_PX / 4.0;
        assert_eq!(snap([302.0, 150.0, 50.0, 50.0], Edges::ALL, &[B], thr).offset[0], 0.0);
        assert_eq!(snap([300.5, 150.0, 50.0, 50.0], Edges::ALL, &[B], thr).offset[0], -0.5);
    }

    #[test]
    fn equal_spacing_continues_a_row() {
        // A and B have a 100 gap; a box at x 403 snaps to 400 (100 after B), beyond 4 but
        // within 1.5 × 4.
        let s = snap([405.0, 0.0, 100.0, 100.0], Edges::ALL, &[A, B], 4.0);
        assert_eq!(s.offset[0], -5.0);
        let ticks: Vec<&Line> = s.guides.iter().filter(|g| g.ticks).collect();
        assert_eq!(ticks.len(), 2);
        assert!(ticks.iter().any(|g| g.a[0] == 300.0 && g.b[0] == 400.0));
        assert!(ticks.iter().any(|g| g.a[0] == 100.0 && g.b[0] == 200.0));
    }

    #[test]
    fn equal_spacing_centres_between_neighbours() {
        // A box 50 wide between A (ends 100) and B (starts 200): centred at 125.
        let s = snap([127.0, 20.0, 50.0, 50.0], Edges::ALL, &[A, B], 4.0);
        assert_eq!(s.offset[0], -2.0);
        assert_eq!(s.guides.iter().filter(|g| g.ticks).count(), 2);
    }

    #[test]
    fn resize_snaps_only_the_moving_edge() {
        // Right edge (handle 3) at 198 snaps to B's left edge 200; the left edge stays.
        let r = [0.0, 200.0, 198.0, 50.0];
        let e = Edges::of_handle(3);
        let s = snap(r, e, &[B], 4.0);
        assert_eq!(s.offset, [2.0, 0.0]);
        assert_eq!(apply(r, e, s.offset), [0.0, 200.0, 200.0, 50.0]);
        // Left handle: the left edge moves, the width compensates.
        let e = Edges::of_handle(7);
        let s = snap([202.0, 200.0, 50.0, 50.0], e, &[B], 4.0);
        assert_eq!(apply([202.0, 200.0, 50.0, 50.0], e, s.offset), [200.0, 200.0, 52.0, 50.0]);
    }

    #[test]
    fn no_candidates_no_snap() {
        let s = snap([3.0, 3.0, 10.0, 10.0], Edges::ALL, &[], 4.0);
        assert_eq!(s, Snap::default());
    }

    #[test]
    fn guides_after_a_nudge_show_exact_alignment() {
        let g = guides([0.0, 300.0, 100.0, 50.0], Edges::ALL, &[A]);
        // Left, centre and right all line up with A.
        assert_eq!(g.iter().filter(|l| !l.ticks && l.a[0] == l.b[0]).count(), 3);
        assert!(guides([1.0, 300.0, 100.0, 50.0], Edges::ALL, &[A]).is_empty());
    }

    #[test]
    fn measures_gaps_and_insets() {
        let m = measure(A, B);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].1, 100.0);
        assert_eq!(m[0].0.a, [100.0, 50.0]);
        // Inside a frame: four insets.
        let m = measure([10.0, 20.0, 50.0, 50.0], [0.0, 0.0, 100.0, 100.0]);
        let mut d: Vec<f32> = m.iter().map(|x| x.1).collect();
        d.sort_by(f32::total_cmp);
        assert_eq!(d, vec![10.0, 20.0, 30.0, 40.0]);
    }
}
