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
    /// Chat color (RGBA8, alpha = dimming); white when the agent has no chat.
    pub color: Option<u32>,
}

/// One agent-activity effect in world space (UX-V1 §4), drawn in its chat color.
#[derive(Clone, Debug, PartialEq)]
pub struct ActivityFx {
    pub rect: [f32; 4],
    /// An [`OverlayPrim`] activity shape, or `RECT` for a static outline (reduce motion).
    pub shape: u32,
    /// Chat color, opaque.
    pub color: u32,
    pub t0: f32,
    pub dur: f32,
    pub seed: f32,
    /// Rain only: width of the frame's title label (CSS px), skipped by the dots.
    pub label_px: f32,
}

/// `#rgb`, `#rrggbb` or `#rrggbbaa` → RGBA8 (red in the low byte).
pub fn parse_color(s: &str) -> Option<u32> {
    let h = s.trim().strip_prefix('#')?;
    let n = |i: usize, len: usize| u8::from_str_radix(h.get(i..i + len)?, 16).ok();
    match h.len() {
        3 => {
            let c = |i| n(i, 1).map(|v| v * 17);
            Some(rgba(c(0)?, c(1)?, c(2)?, 255))
        }
        6 => Some(rgba(n(0, 2)?, n(2, 2)?, n(4, 2)?, 255)),
        8 => Some(rgba(n(0, 2)?, n(2, 2)?, n(4, 2)?, n(6, 2)?)),
        _ => None,
    }
}

/// `c` with its alpha replaced.
pub const fn with_alpha(c: u32, a: u8) -> u32 {
    (c & 0x00ff_ffff) | (a as u32) << 24
}

/// A node's provenance badges (docs/ASSETS-V1.md §5).
#[derive(Clone, Debug, PartialEq)]
pub struct BadgeView {
    pub rect: [f32; 4],
    /// `OverlayPrim::BADGE_*` source glyph (none for nodes without an asset).
    pub source: Option<u32>,
    /// An import that is uploaded to fal (`↑` dot).
    pub uploaded: bool,
    /// `role: final` (`★`).
    pub final_role: bool,
    /// Hovered or selected: shown at any size (else only on big tiles).
    pub force: bool,
}

/// Source badge + uploaded flag of an asset record (JSON, so both the current and the
/// Assets-v1 shapes work): `source` (import | generate | derive | render | capture), else the
/// lineage kind, else the old generation lineage (`model: "import"` = an import).
pub fn badge_of(asset: &serde_json::Value) -> (u32, bool) {
    let lin = asset.get("lineage");
    let src = asset
        .get("source")
        .and_then(|v| v.as_str().or_else(|| v.get("kind").and_then(|k| k.as_str())))
        .or_else(|| lin.and_then(|l| l.get("kind")).and_then(|k| k.as_str()))
        .unwrap_or_else(|| match lin.and_then(|l| l.get("model")).and_then(|m| m.as_str()) {
            None | Some("import") | Some("") => "import",
            Some(_) => "generate",
        });
    let glyph = match src {
        "generate" => OverlayPrim::BADGE_GENERATE,
        "derive" => OverlayPrim::BADGE_DERIVE,
        "render" => OverlayPrim::BADGE_RENDER,
        _ => OverlayPrim::BADGE_IMPORT,
    };
    let fal = asset.get("fal");
    let uploaded = fal.is_some_and(|f| f.get("synced").and_then(|v| v.as_bool()) == Some(true) || f.get("assetRecordId").is_some_and(|v| !v.is_null()))
        || (glyph == OverlayPrim::BADGE_IMPORT && asset.get("uri").and_then(|u| u.as_str()).is_some_and(|u| u.starts_with("https://")));
    (glyph, uploaded)
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
    /// Agent activity effects (drawn under the selection).
    pub activity: &'a [ActivityFx],
    /// Rects highlighted from outside the canvas (layers row hover).
    pub highlight: &'a [[f32; 4]],
    /// Provenance badges.
    pub badges: &'a [BadgeView],
    /// Lineage view: nodes outside the selected node's lineage, dimmed.
    pub dim: &'a [[f32; 4]],
    pub dpr: f32,
    /// World units per physical pixel (1 / zoom).
    pub px_world: f32,
}

pub fn build(i: &OverlayInput) -> Vec<OverlayPrim> {
    let d = i.dpr;
    let p = i.palette;
    let mut out = Vec::new();

    if !i.dim.is_empty() {
        let b = p.bg;
        let veil = rgba((b[0] * 255.0) as u8, (b[1] * 255.0) as u8, (b[2] * 255.0) as u8, 200);
        for r in i.dim {
            out.push(OverlayPrim::rect(*r, veil, 0, 0.0));
        }
    }
    for r in i.outlines {
        out.push(OverlayPrim::rect(*r, 0, p.line, d));
    }
    let pad = 3.0 * d * i.px_world;
    for r in i.claims {
        out.push(OverlayPrim::dashed([r[0] - pad, r[1] - pad, r[2] + 2.0 * pad, r[3] + 2.0 * pad], 0, p.dim, d));
    }
    for f in i.activity {
        let stroke = with_alpha(f.color, 220);
        let prim = match f.shape {
            // The strip starts after the frame's title (`label_px`, CSS px) so dots never cross the text.
            OverlayPrim::RAIN => OverlayPrim {
                world: [f.rect[0], f.rect[1]],
                world_size: [f.rect[2], 0.0],
                px_offset: [f.label_px * d, -12.0 * d],
                px_size: [-f.label_px * d, 9.0 * d],
                fill: with_alpha(f.color, 200),
                shape: OverlayPrim::RAIN,
                t0: f.t0,
                dur: f.dur,
                seed: f.seed,
                ..OverlayPrim::rect([0.0; 4], 0, 0, 0.0)
            },
            OverlayPrim::RECT => OverlayPrim::rect(f.rect, 0, with_alpha(f.color, 150), d),
            shape => {
                let fill = match shape {
                    OverlayPrim::GHOST => with_alpha(f.color, 56),
                    OverlayPrim::FLASH => with_alpha(f.color, 60),
                    _ => with_alpha(f.color, 96),
                };
                OverlayPrim { seed: f.seed, ..OverlayPrim::activity(f.rect, shape, fill, stroke, d, f.t0, f.dur) }
            }
        };
        out.push(prim);
    }
    for r in i.highlight {
        out.push(OverlayPrim::rect(*r, 0, (p.ink & 0x00ff_ffff) | (140 << 24), d));
    }
    if let Some(h) = i.hover.and_then(|h| i.scene.get(h))
        && !i.selection.iter().any(|s| s == &h.id)
    {
        // Subtle: clearly weaker than the selection outline.
        out.push(OverlayPrim::rect(h.rect, 0, (p.ink & 0x00ff_ffff) | (100 << 24), d));
    }
    // Badges: tiny, dim glyphs at the top-left corner.
    let px = 11.0 * d;
    let fill = with_alpha(p.panel, 170);
    for b in i.badges {
        let size = if b.force { [1e9, 1e9] } else { [b.rect[2], b.rect[3]] };
        let mut slot = 0.0;
        if let Some(g) = b.source {
            out.push(OverlayPrim::glyph(b.rect, size, g, slot, px, [fill, p.dim], d));
            slot += 1.0;
            if b.uploaded {
                let mut dot = OverlayPrim::glyph(b.rect, size, OverlayPrim::BADGE_UPLOADED, 0.0, 5.0 * d, [0, p.dim], d);
                dot.px_offset = [3.0 * d + slot * (px + d) - d, 3.0 * d];
                out.push(dot);
                slot += 0.5;
            }
        }
        if b.final_role {
            out.push(OverlayPrim::glyph(b.rect, size, OverlayPrim::BADGE_FINAL, slot, px, [fill, p.ink], d));
        }
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
        let stroke = c.color.map_or(BLACK, |col| with_alpha(BLACK, ((col >> 24) as f32 * 0.8) as u8));
        out.push(OverlayPrim::cursor(c.world, 13.0 * d, c.color.unwrap_or(WHITE), stroke));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_css_colors() {
        assert_eq!(parse_color("#fff"), Some(rgba(255, 255, 255, 255)));
        assert_eq!(parse_color("#7b93c4"), Some(rgba(0x7b, 0x93, 0xc4, 255)));
        assert_eq!(parse_color("#7b93c480"), Some(rgba(0x7b, 0x93, 0xc4, 0x80)));
        assert_eq!(parse_color("blue"), None);
        assert_eq!(parse_color("#12"), None);
    }

    #[test]
    fn badges_from_old_and_new_assets() {
        use serde_json::json;
        assert_eq!(badge_of(&json!({"id": "x"})), (OverlayPrim::BADGE_IMPORT, false));
        assert_eq!(badge_of(&json!({"lineage": {"model": "import", "actor": "you"}})).0, OverlayPrim::BADGE_IMPORT);
        assert_eq!(badge_of(&json!({"lineage": {"model": "fal-ai/flux", "actor": "a"}})).0, OverlayPrim::BADGE_GENERATE);
        assert_eq!(badge_of(&json!({"source": "derive"})).0, OverlayPrim::BADGE_DERIVE);
        assert_eq!(badge_of(&json!({"source": "render"})).0, OverlayPrim::BADGE_RENDER);
        assert_eq!(badge_of(&json!({"lineage": {"kind": "derive", "op": "crop"}})).0, OverlayPrim::BADGE_DERIVE);
        assert_eq!(badge_of(&json!({"source": "import", "fal": {"synced": true}})), (OverlayPrim::BADGE_IMPORT, true));
        assert!(!badge_of(&json!({"source": "generate", "uri": "https://v3.fal.media/x.png"})).1);
    }

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
