//! Builds the engine overlay (canvas-level UI) from client state: hover and selection
//! outlines, resize handles, marquee, annotation pins/boxes, the annotation being drafted, and
//! remote cursors. Rebuilt whenever that state changes; the engine skips identical uploads.

use falos_engine::OverlayPrim;

use crate::interact::handle_points;
use crate::scene::Scene;

/// RGBA8 packed with red in the low byte (the engine's color format).
pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> u32 {
    r as u32 | (g as u32) << 8 | (b as u32) << 16 | (a as u32) << 24
}

pub const ACCENT: u32 = rgba(76, 141, 255, 255);
const ACCENT_FILL: u32 = rgba(76, 141, 255, 28);
const WHITE: u32 = rgba(255, 255, 255, 255);
const SHADOW: u32 = rgba(0, 0, 0, 90);

/// Parses `#rrggbb` (or `#rgb`) into the engine color format.
pub fn parse_color(s: &str, alpha: u8) -> Option<u32> {
    let h = s.strip_prefix('#')?;
    let v = |i: usize, n: usize| u8::from_str_radix(&h[i..i + n], 16).ok();
    match h.len() {
        6 => Some(rgba(v(0, 2)?, v(2, 2)?, v(4, 2)?, alpha)),
        3 => Some(rgba(v(0, 1)? * 17, v(1, 1)? * 17, v(2, 1)? * 17, alpha)),
        _ => None,
    }
}

/// Pin color by annotation status.
pub fn status_color(status: &str, alpha: u8) -> u32 {
    match status {
        "in_progress" => rgba(245, 165, 36, alpha),
        "resolved" => rgba(63, 185, 80, alpha),
        _ => rgba(139, 92, 246, alpha),
    }
}

pub struct PinView {
    pub world: [f32; 2],
    /// Box anchor in world units, if any.
    pub region: Option<[f32; 4]>,
    pub status: String,
    pub selected: bool,
    pub hovered: bool,
}

pub struct CursorView {
    pub world: [f32; 2],
    pub color: u32,
}

pub struct OverlayInput<'a> {
    pub scene: &'a Scene,
    pub selection: &'a [String],
    pub hover: Option<&'a str>,
    pub show_handles: bool,
    pub marquee: Option<[f32; 4]>,
    /// Annotation box being dragged out.
    pub drawing: Option<[f32; 4]>,
    pub pins: &'a [PinView],
    /// The unsent annotation: anchor point and optional box (world).
    pub draft: Option<([f32; 2], Option<[f32; 4]>)>,
    pub cursors: &'a [CursorView],
    pub dpr: f32,
}

pub const PIN_PX: f32 = 26.0;

pub fn build(i: &OverlayInput) -> Vec<OverlayPrim> {
    let d = i.dpr;
    let mut out = Vec::new();

    if let Some(h) = i.hover.and_then(|h| i.scene.get(h))
        && !i.selection.iter().any(|s| s == &h.id)
    {
        out.push(OverlayPrim::rect(h.rect, 0, ACCENT, 1.5 * d));
    }
    for s in i.selection.iter().filter_map(|id| i.scene.get(id)) {
        out.push(OverlayPrim::rect(s.rect, 0, ACCENT, 1.5 * d));
    }
    if i.selection.len() > 1
        && let Some(b) = i.scene.bounds_of(i.selection)
    {
        out.push(OverlayPrim::rect(b, 0, ACCENT, 1.0 * d));
    }
    if i.show_handles
        && i.selection.len() == 1
        && let Some(n) = i.scene.get(&i.selection[0])
    {
        for p in handle_points(n.rect) {
            out.push(OverlayPrim::handle(p, 8.0 * d, WHITE, ACCENT, 1.5 * d));
        }
    }

    for p in i.pins {
        if let Some(r) = p.region {
            let strong = p.selected || p.hovered;
            out.push(OverlayPrim::rect(
                r,
                status_color(&p.status, if strong { 36 } else { 0 }),
                status_color(&p.status, if strong { 255 } else { 150 }),
                (if strong { 2.0 } else { 1.25 }) * d,
            ));
        }
    }
    for p in i.pins.iter().filter(|p| !p.selected) {
        push_pin(&mut out, p.world, &p.status, p.hovered, d);
    }
    // The selected pin draws last, on top.
    for p in i.pins.iter().filter(|p| p.selected) {
        push_pin(&mut out, p.world, &p.status, true, d);
    }

    if let Some(r) = i.marquee {
        out.push(OverlayPrim::dashed(r, ACCENT_FILL, ACCENT, 1.0 * d));
    }
    if let Some(r) = i.drawing {
        out.push(OverlayPrim::dashed(r, rgba(139, 92, 246, 30), rgba(139, 92, 246, 255), 1.5 * d));
    }
    if let Some((p, region)) = i.draft {
        if let Some(r) = region {
            out.push(OverlayPrim::dashed(r, rgba(139, 92, 246, 30), rgba(139, 92, 246, 255), 1.5 * d));
        }
        let px = PIN_PX * d;
        out.push(shadowed(OverlayPrim::pin(p, px, WHITE, rgba(139, 92, 246, 255), 2.5 * d), d));
        out.push(OverlayPrim::pin(p, px, WHITE, rgba(139, 92, 246, 255), 2.5 * d));
    }

    for c in i.cursors {
        out.push(OverlayPrim::cursor(c.world, 20.0 * d, c.color, WHITE));
    }
    out
}

fn shadowed(mut p: OverlayPrim, d: f32) -> OverlayPrim {
    p.px_offset[1] += 1.5 * d;
    p.fill = SHADOW;
    p.stroke = SHADOW;
    p
}

fn push_pin(out: &mut Vec<OverlayPrim>, world: [f32; 2], status: &str, big: bool, d: f32) {
    let px = if big { PIN_PX + 4.0 } else { PIN_PX } * d;
    let pin = OverlayPrim::pin(world, px, status_color(status, if status == "resolved" { 200 } else { 255 }), WHITE, 2.0 * d);
    out.push(shadowed(pin, d));
    out.push(pin);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        assert_eq!(parse_color("#ff0000", 255), Some(rgba(255, 0, 0, 255)));
        assert_eq!(parse_color("#0f0", 128), Some(rgba(0, 255, 0, 128)));
        assert_eq!(parse_color("red", 255), None);
    }
}
