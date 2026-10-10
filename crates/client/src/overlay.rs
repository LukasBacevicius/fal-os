//! Builds the engine overlay (canvas-level UI) from client state: 1 px outlines for frames and
//! cards, hover and selection outlines, square resize handles, the marquee, the region being
//! Alt-dragged for chat context, claims, and agent cursors. Monochrome (UI-SPEC.md): the only
//! colors are the theme's ink, line and dim grays. Rebuilt whenever that state changes; the
//! engine skips identical uploads.

use falos_engine::OverlayPrim;

use crate::interact::handle_points;
use crate::scene::Scene;

/// RGBA8 packed with red in the low byte (the engine's color format).
pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> u32 {
    r as u32 | (g as u32) << 8 | (b as u32) << 16 | (a as u32) << 24
}

const fn gray(v: u8, a: u8) -> u32 {
    rgba(v, v, v, a)
}

/// Theme colors (mirrors the CSS variables in apps/web/src/styles.css).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub bg: [f64; 4],
    /// `--panel`: card fill.
    pub panel: u32,
    /// `--line`: 1 px borders.
    pub line: u32,
    /// `--dim`: secondary marks (hover, claims).
    pub dim: u32,
    /// `--text`: selection, marquee.
    pub ink: u32,
    /// Generating tile fill.
    pub tile: u32,
    pub light: bool,
}

pub const DARK: Palette = Palette {
    bg: [10.0 / 255.0, 10.0 / 255.0, 10.0 / 255.0, 1.0],
    panel: gray(15, 255),
    line: gray(38, 255),
    dim: gray(138, 255),
    ink: gray(230, 255),
    tile: rgba(26, 26, 28, 255),
    light: false,
};

pub const LIGHT: Palette = Palette {
    bg: [1.0, 1.0, 1.0, 1.0],
    panel: gray(247, 255),
    line: gray(217, 255),
    dim: gray(110, 255),
    ink: gray(10, 255),
    tile: rgba(236, 236, 238, 255),
    light: true,
};

const WHITE: u32 = gray(255, 255);
const BLACK: u32 = gray(0, 255);

pub struct CursorView {
    pub world: [f32; 2],
}

pub struct OverlayInput<'a> {
    pub scene: &'a Scene,
    pub palette: &'a Palette,
    pub selection: &'a [String],
    pub hover: Option<&'a str>,
    pub show_handles: bool,
    pub marquee: Option<[f32; 4]>,
    /// Region being Alt-dragged on a media node (world).
    pub pick: Option<[f32; 4]>,
    /// World rects that get a plain 1 px line (frames, cards).
    pub outlines: &'a [[f32; 4]],
    /// World rects claimed by other sessions.
    pub claims: &'a [[f32; 4]],
    pub cursors: &'a [CursorView],
    pub dpr: f32,
    /// World units per physical pixel (1 / zoom).
    pub px_world: f32,
}

pub fn build(i: &OverlayInput) -> Vec<OverlayPrim> {
    let d = i.dpr;
    let p = i.palette;
    let mut out = Vec::new();

    for r in i.outlines {
        out.push(OverlayPrim::rect(*r, 0, p.line, d));
    }
    let pad = 3.0 * d * i.px_world;
    for r in i.claims {
        out.push(OverlayPrim::dashed([r[0] - pad, r[1] - pad, r[2] + 2.0 * pad, r[3] + 2.0 * pad], 0, p.dim, d));
    }
    if let Some(h) = i.hover.and_then(|h| i.scene.get(h))
        && !i.selection.iter().any(|s| s == &h.id)
    {
        // Subtle: clearly weaker than the selection outline.
        out.push(OverlayPrim::rect(h.rect, 0, (p.ink & 0x00ff_ffff) | (100 << 24), d));
    }
    for s in i.selection.iter().filter_map(|id| i.scene.get(id)) {
        out.push(OverlayPrim::rect(s.rect, 0, p.ink, d));
    }
    if i.selection.len() > 1
        && let Some(b) = i.scene.bounds_of(i.selection)
    {
        out.push(OverlayPrim::dashed(b, 0, p.dim, d));
    }
    if i.show_handles
        && i.selection.len() == 1
        && let Some(n) = i.scene.get(&i.selection[0])
    {
        for hp in handle_points(n.rect) {
            let mut h = OverlayPrim::handle(hp, 6.0 * d, WHITE, BLACK, d);
            h.radius_px = 0.0;
            out.push(h);
        }
    }
    if let Some(r) = i.marquee {
        out.push(OverlayPrim::dashed(r, (p.ink & 0x00ff_ffff) | (12 << 24), p.ink, d));
    }
    if let Some(r) = i.pick {
        out.push(OverlayPrim::dashed(r, (p.ink & 0x00ff_ffff) | (20 << 24), p.ink, d));
    }
    for c in i.cursors {
        out.push(OverlayPrim::cursor(c.world, 13.0 * d, WHITE, BLACK));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_is_monochrome() {
        for p in [DARK, LIGHT] {
            for c in [p.panel, p.line, p.dim, p.ink] {
                let (r, g, b) = (c & 255, (c >> 8) & 255, (c >> 16) & 255);
                assert!(r == g && g == b, "{c:x} is not gray");
            }
        }
    }
}
