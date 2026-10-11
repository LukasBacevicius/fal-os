//! Geometry: 2×3 transforms, Figma's path blobs, vector networks and synthesized shapes, all
//! as SVG path data (`M L Q C Z` with absolute coordinates) that `Path2D` accepts.

use std::fmt::Write;

/// Affine transform `[m00, m01, m02, m10, m11, m12]`: `x' = m00·x + m01·y + m02`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mat(pub [f64; 6]);

impl Mat {
    pub const ID: Mat = Mat([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);

    pub fn translate(x: f64, y: f64) -> Mat {
        Mat([1.0, 0.0, x, 0.0, 1.0, y])
    }

    pub fn scale(sx: f64, sy: f64) -> Mat {
        Mat([sx, 0.0, 0.0, 0.0, sy, 0.0])
    }

    /// `self ∘ o`: apply `o` first, then `self`.
    pub fn mul(&self, o: &Mat) -> Mat {
        let [a, b, c, d, e, f] = self.0;
        let [g, h, i, j, k, l] = o.0;
        Mat([a * g + b * j, a * h + b * k, a * i + b * l + c, d * g + e * j, d * h + e * k, d * i + e * l + f])
    }

    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let [a, b, c, d, e, f] = self.0;
        (a * x + b * y + c, d * x + e * y + f)
    }

    pub fn invert(&self) -> Option<Mat> {
        let [a, b, c, d, e, f] = self.0;
        let det = a * e - b * d;
        if det.abs() < 1e-12 || !det.is_finite() {
            return None;
        }
        let (ia, ib, id, ie) = (e / det, -b / det, -d / det, a / det);
        Some(Mat([ia, ib, -(ia * c + ib * f), id, ie, -(id * c + ie * f)]))
    }

    /// No rotation/skew (scale and translation only).
    pub fn axis_aligned(&self) -> bool {
        self.0[1].abs() < 1e-6 && self.0[3].abs() < 1e-6
    }

    /// Uniform-ish scale factor (sqrt of the determinant).
    pub fn scale_factor(&self) -> f64 {
        (self.0[0] * self.0[4] - self.0[1] * self.0[3]).abs().sqrt()
    }

    /// Axis-aligned bounds `[x, y, w, h]` of the box `[0,w]×[0,h]` under this transform.
    pub fn bounds(&self, w: f64, h: f64) -> [f64; 4] {
        let pts = [self.apply(0.0, 0.0), self.apply(w, 0.0), self.apply(0.0, h), self.apply(w, h)];
        let x0 = pts.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
        let y0 = pts.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
        let x1 = pts.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max);
        let y1 = pts.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
        [x0, y0, x1 - x0, y1 - y0]
    }
}

/// Shortest decimal form (2 decimals are plenty for screen raster work).
fn num(out: &mut String, v: f64) {
    let v = if v.is_finite() { v } else { 0.0 };
    let r = (v * 100.0).round() / 100.0;
    if r == r.trunc() {
        let _ = write!(out, "{}", r as i64);
    } else {
        let _ = write!(out, "{r}");
    }
}

struct PathBuf(String);

impl PathBuf {
    fn new() -> Self {
        PathBuf(String::new())
    }
    fn cmd(&mut self, c: char, pts: &[f64]) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        self.0.push(c);
        for v in pts {
            self.0.push(' ');
            num(&mut self.0, *v);
        }
    }
}

fn f32_at(b: &[u8], i: usize) -> Option<f64> {
    Some(f32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?) as f64)
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

/// A `commandsBlob` (fill/stroke geometry): a command byte followed by f32 coordinates:
/// `0` close, `1` move (x y), `2` line (x y), `3` quad (x1 y1 x y), `4` cubic (x1 y1 x2 y2 x y).
pub fn commands_to_svg(blob: &[u8]) -> Option<String> {
    let mut p = PathBuf::new();
    let mut i = 0;
    while i < blob.len() {
        let c = blob[i];
        i += 1;
        let (letter, n) = match c {
            0 => ('Z', 0),
            1 => ('M', 2),
            2 => ('L', 2),
            3 => ('Q', 4),
            4 => ('C', 6),
            _ => return None,
        };
        let mut v = [0.0; 6];
        for (k, slot) in v.iter_mut().enumerate().take(n) {
            *slot = f32_at(blob, i + k * 4)?;
        }
        i += n * 4;
        p.cmd(letter, &v[..n]);
    }
    Some(p.0)
}

/// A decoded `vectorNetworkBlob` (coordinates in the network's normalized space).
#[derive(Debug, Default, Clone)]
pub struct Network {
    pub vertices: Vec<(f64, f64)>,
    /// `(start, start tangent, end, end tangent)`.
    pub segments: Vec<(usize, (f64, f64), usize, (f64, f64))>,
    /// `(nonzero winding?, loops of segment indices)`.
    pub regions: Vec<(bool, Vec<Vec<usize>>)>,
}

/// Layout: `u32` vertex, segment and region counts; vertices `(u32 style, f32 x, f32 y)`;
/// segments `(u32 style, u32 start, f32 dx, f32 dy, u32 end, f32 dx, f32 dy)`; regions
/// `(u32 style << 1 | nonzero, u32 loop count, (u32 n, u32 segment × n)*)`.
pub fn decode_network(b: &[u8]) -> Option<Network> {
    let nv = u32_at(b, 0)? as usize;
    let ns = u32_at(b, 4)? as usize;
    let nr = u32_at(b, 8)? as usize;
    if nv.saturating_mul(12).saturating_add(ns.saturating_mul(28)) > b.len() {
        return None;
    }
    let mut net = Network::default();
    let mut i = 12;
    for _ in 0..nv {
        net.vertices.push((f32_at(b, i + 4)?, f32_at(b, i + 8)?));
        i += 12;
    }
    for _ in 0..ns {
        let s = u32_at(b, i + 4)? as usize;
        let e = u32_at(b, i + 16)? as usize;
        if s >= nv || e >= nv {
            return None;
        }
        net.segments.push((s, (f32_at(b, i + 8)?, f32_at(b, i + 12)?), e, (f32_at(b, i + 20)?, f32_at(b, i + 24)?)));
        i += 28;
    }
    for _ in 0..nr {
        let style = u32_at(b, i)?;
        let nl = u32_at(b, i + 4)? as usize;
        i += 8;
        let mut loops = Vec::new();
        for _ in 0..nl {
            let n = u32_at(b, i)? as usize;
            i += 4;
            let mut l = Vec::with_capacity(n.min(4096));
            for _ in 0..n {
                let s = u32_at(b, i)? as usize;
                if s >= ns {
                    return None;
                }
                l.push(s);
                i += 4;
            }
            loops.push(l);
        }
        net.regions.push((style & 1 == 1, loops));
    }
    Some(net)
}

impl Network {
    fn seg(&self, p: &mut PathBuf, m: &Mat, idx: usize, reverse: bool, first: bool) {
        let (s, ts, e, te) = self.segments[idx];
        let (a, ta, b, tb) = if reverse { (e, te, s, ts) } else { (s, ts, e, te) };
        let pa = self.vertices[a];
        let pb = self.vertices[b];
        if first {
            let (x, y) = m.apply(pa.0, pa.1);
            p.cmd('M', &[x, y]);
        }
        let (x, y) = m.apply(pb.0, pb.1);
        if ta == (0.0, 0.0) && tb == (0.0, 0.0) {
            p.cmd('L', &[x, y]);
        } else {
            let (x1, y1) = m.apply(pa.0 + ta.0, pa.1 + ta.1);
            let (x2, y2) = m.apply(pb.0 + tb.0, pb.1 + tb.1);
            p.cmd('C', &[x1, y1, x2, y2, x, y]);
        }
    }

    /// Chains segments (each walked forward or backward so consecutive ones share a vertex)
    /// into subpaths; `close` closes each chain that returns to its start.
    fn chain(&self, p: &mut PathBuf, m: &Mat, order: &[usize], close_loops: bool) {
        let mut cur: Option<usize> = None;
        let mut start = 0;
        for (k, &idx) in order.iter().enumerate() {
            let (s, _, e, _) = self.segments[idx];
            let reverse = match cur {
                Some(c) if c == s => false,
                Some(c) if c == e => true,
                // A new chain: orient by the next segment when it connects at our start.
                _ => order.get(k + 1).is_some_and(|&n| {
                    let (ns, _, ne, _) = self.segments[n];
                    (s == ns || s == ne) && e != ns && e != ne
                }),
            };
            let first = !matches!(cur, Some(c) if c == s || c == e);
            if first {
                if close_loops && cur == Some(start) {
                    p.cmd('Z', &[]);
                }
                start = if reverse { e } else { s };
            }
            self.seg(p, m, idx, reverse, first);
            cur = Some(if reverse { s } else { e });
        }
        if close_loops && cur == Some(start) && !order.is_empty() {
            p.cmd('Z', &[]);
        }
    }

    /// Fill path: the regions (each loop closed), else every segment chained. `m` maps the
    /// normalized network space to node space. Returns `(path, nonzero)` per region.
    pub fn fill_paths(&self, m: &Mat) -> Vec<(String, bool)> {
        if self.regions.is_empty() {
            if self.segments.is_empty() {
                return vec![];
            }
            let mut p = PathBuf::new();
            let order: Vec<usize> = (0..self.segments.len()).collect();
            self.chain(&mut p, m, &order, true);
            return vec![(p.0, true)];
        }
        self.regions
            .iter()
            .map(|(nonzero, loops)| {
                let mut p = PathBuf::new();
                for l in loops {
                    self.chain(&mut p, m, l, true);
                    if !p.0.ends_with('Z') {
                        p.cmd('Z', &[]);
                    }
                }
                (p.0, *nonzero)
            })
            .collect()
    }

    /// Stroke path: every segment, chained into open (or closed) subpaths.
    pub fn stroke_path(&self, m: &Mat) -> String {
        let mut p = PathBuf::new();
        let order: Vec<usize> = (0..self.segments.len()).collect();
        self.chain(&mut p, m, &order, true);
        p.0
    }
}

/// A rectangle with per-corner radii `[tl, tr, br, bl]` (clamped to fit).
pub fn rect_path(w: f64, h: f64, r: [f64; 4]) -> String {
    let mut p = PathBuf::new();
    let max = (w.min(h) / 2.0).max(0.0);
    let [tl, tr, br, bl] = r.map(|v| v.clamp(0.0, max));
    // Circular arcs as cubics (kappa).
    const K: f64 = 0.552_284_75;
    p.cmd('M', &[tl, 0.0]);
    p.cmd('L', &[w - tr, 0.0]);
    if tr > 0.0 {
        p.cmd('C', &[w - tr + tr * K, 0.0, w, tr - tr * K, w, tr]);
    }
    p.cmd('L', &[w, h - br]);
    if br > 0.0 {
        p.cmd('C', &[w, h - br + br * K, w - br + br * K, h, w - br, h]);
    }
    p.cmd('L', &[bl, h]);
    if bl > 0.0 {
        p.cmd('C', &[bl - bl * K, h, 0.0, h - bl + bl * K, 0.0, h - bl]);
    }
    p.cmd('L', &[0.0, tl]);
    if tl > 0.0 {
        p.cmd('C', &[0.0, tl - tl * K, tl - tl * K, 0.0, tl, 0.0]);
    }
    p.cmd('Z', &[]);
    p.0
}

pub fn ellipse_path(w: f64, h: f64) -> String {
    const K: f64 = 0.552_284_75;
    let (rx, ry) = (w / 2.0, h / 2.0);
    let mut p = PathBuf::new();
    p.cmd('M', &[w, ry]);
    p.cmd('C', &[w, ry + ry * K, rx + rx * K, h, rx, h]);
    p.cmd('C', &[rx - rx * K, h, 0.0, ry + ry * K, 0.0, ry]);
    p.cmd('C', &[0.0, ry - ry * K, rx - rx * K, 0.0, rx, 0.0]);
    p.cmd('C', &[rx + rx * K, 0.0, w, ry - ry * K, w, ry]);
    p.cmd('Z', &[]);
    p.0
}

/// Regular polygon (`inner` = None) or star with `count` points inscribed in the box.
pub fn polygon_path(w: f64, h: f64, count: usize, inner: Option<f64>) -> String {
    let count = count.clamp(3, 200);
    let mut p = PathBuf::new();
    let steps = if inner.is_some() { count * 2 } else { count };
    for k in 0..steps {
        let a = -std::f64::consts::FRAC_PI_2 + k as f64 * std::f64::consts::TAU / steps as f64;
        let r = if inner.is_some() && k % 2 == 1 { inner.unwrap_or(0.5) } else { 1.0 };
        let (x, y) = (w / 2.0 + a.cos() * r * w / 2.0, h / 2.0 + a.sin() * r * h / 2.0);
        p.cmd(if k == 0 { 'M' } else { 'L' }, &[x, y]);
    }
    p.cmd('Z', &[]);
    p.0
}

pub fn line_path(w: f64) -> String {
    let mut p = PathBuf::new();
    p.cmd('M', &[0.0, 0.0]);
    p.cmd('L', &[w, 0.0]);
    p.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(cmds: &[(u8, &[f32])]) -> Vec<u8> {
        let mut b = Vec::new();
        for (c, v) in cmds {
            b.push(*c);
            for x in *v {
                b.extend_from_slice(&x.to_le_bytes());
            }
        }
        b
    }

    #[test]
    fn decodes_commands() {
        let b = blob(&[(1, &[0.0, 4.0]), (2, &[15.5, 0.0]), (4, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]), (3, &[1.0, 1.0, 2.0, 2.0]), (0, &[])]);
        assert_eq!(commands_to_svg(&b).unwrap(), "M 0 4 L 15.5 0 C 1 2 3 4 5 6 Q 1 1 2 2 Z");
        assert!(commands_to_svg(&[9]).is_none());
        assert!(commands_to_svg(&[1, 0, 0]).is_none());
    }

    #[test]
    fn transforms() {
        let t = Mat([1.0, 0.0, 10.0, 0.0, 1.0, 20.0]);
        let s = Mat::scale(2.0, 3.0);
        assert_eq!(t.mul(&s).apply(1.0, 1.0), (12.0, 23.0));
        let inv = t.mul(&s).invert().unwrap();
        let (x, y) = inv.apply(12.0, 23.0);
        assert!((x - 1.0).abs() < 1e-9 && (y - 1.0).abs() < 1e-9);
        // 90° rotation: bounds of a 10×20 box.
        let r = Mat([0.0, -1.0, 0.0, 1.0, 0.0, 0.0]);
        assert_eq!(r.bounds(10.0, 20.0), [-20.0, 0.0, 20.0, 10.0]);
    }

    fn network_blob(v: &[(f32, f32)], s: &[(u32, u32)], regions: &[(u32, Vec<Vec<u32>>)]) -> Vec<u8> {
        let mut b = Vec::new();
        for n in [v.len(), s.len(), regions.len()] {
            b.extend_from_slice(&(n as u32).to_le_bytes());
        }
        for (x, y) in v {
            b.extend_from_slice(&0u32.to_le_bytes());
            b.extend_from_slice(&x.to_le_bytes());
            b.extend_from_slice(&y.to_le_bytes());
        }
        for (a, e) in s {
            b.extend_from_slice(&0u32.to_le_bytes());
            b.extend_from_slice(&a.to_le_bytes());
            b.extend_from_slice(&[0; 8]);
            b.extend_from_slice(&e.to_le_bytes());
            b.extend_from_slice(&[0; 8]);
        }
        for (w, loops) in regions {
            b.extend_from_slice(&w.to_le_bytes());
            b.extend_from_slice(&(loops.len() as u32).to_le_bytes());
            for l in loops {
                b.extend_from_slice(&(l.len() as u32).to_le_bytes());
                for s in l {
                    b.extend_from_slice(&s.to_le_bytes());
                }
            }
        }
        b
    }

    #[test]
    fn vector_network_triangle() {
        // Segments deliberately out of orientation: 0→1, 2→1, 2→0.
        let b = network_blob(&[(0.0, 0.0), (10.0, 0.0), (5.0, 10.0)], &[(0, 1), (2, 1), (2, 0)], &[(1, vec![vec![0, 1, 2]])]);
        let n = decode_network(&b).unwrap();
        let fills = n.fill_paths(&Mat::scale(2.0, 2.0));
        assert_eq!(fills, vec![("M 0 0 L 20 0 L 10 20 L 0 0 Z".to_string(), true)]);
        // Without regions everything is chained.
        let b = network_blob(&[(0.0, 0.0), (10.0, 0.0), (5.0, 10.0)], &[(0, 1), (1, 2)], &[]);
        let n = decode_network(&b).unwrap();
        assert_eq!(n.stroke_path(&Mat::ID), "M 0 0 L 10 0 L 5 10");
        assert!(decode_network(&b[..20]).is_none());
    }

    #[test]
    fn shapes() {
        assert_eq!(rect_path(10.0, 5.0, [0.0; 4]), "M 0 0 L 10 0 L 10 5 L 0 5 L 0 0 Z");
        assert!(rect_path(10.0, 10.0, [2.0; 4]).contains('C'));
        assert!(ellipse_path(10.0, 10.0).starts_with("M 10 5"));
        assert_eq!(polygon_path(10.0, 10.0, 3, None).matches('L').count(), 2);
        assert_eq!(polygon_path(10.0, 10.0, 5, Some(0.4)).matches('L').count(), 9);
    }
}
