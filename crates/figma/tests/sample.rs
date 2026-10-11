//! Decodes a real Figma clipboard capture when one is available (it is private design work,
//! so it is not in the repo): `FALOS_FIGMA_CLIP=/path/to/clip.html cargo test -p falos-figma`.
//! Skipped otherwise; `tests/synthetic.rs` covers the same paths with a generated payload.

use falos_figma::{Kind, plan_html};

const DEFAULT: &str = "/private/tmp/claude-501/-Users-lukas-Development-fal-os/4094f03e-78d8-428d-9f28-59f50ea0c3c5/scratchpad/figma-clip/clip.html";

fn sample() -> Option<String> {
    let path = std::env::var("FALOS_FIGMA_CLIP").unwrap_or_else(|_| DEFAULT.into());
    std::fs::read_to_string(&path).ok()
}

#[test]
fn real_clipboard_board() {
    let Some(html) = sample() else {
        eprintln!("no Figma clipboard sample; skipping");
        return;
    };
    let t = std::time::Instant::now();
    let plan = plan_html(&html).expect("decodes");
    eprintln!("decoded + planned in {:?}", t.elapsed());
    eprintln!("stats: {:?}", plan.stats);
    eprintln!("warnings: {:#?}", plan.warnings);
    eprintln!("size: {:?}, images: {}", plan.size, plan.images.len());
    if std::env::var("FALOS_FIGMA_DUMP").is_ok() {
        let clip = falos_figma::decode_html(&html).unwrap();
        eprintln!("{}", falos_figma::plan::outline(&falos_figma::plan::resolve(&clip)));
        for (i, n) in plan.nodes.iter().enumerate() {
            let depth = std::iter::successors(n.parent, |p| plan.nodes[*p].parent).count();
            let extra = match n.kind {
                Kind::Text => format!("{:?}", n.text.as_ref().map(|t| (&t.characters, t.font_size, &t.color))),
                Kind::Raster => format!("{} ops", n.raster.as_ref().map_or(0, |r| r.ops.len())),
                Kind::Image => n.image.clone().unwrap_or_default(),
                _ => format!("{:?} {:?}", n.fill, n.radius),
            };
            eprintln!("{i:4} {}{:?} {:?} ({}, {}) {}x{} {extra}", "  ".repeat(depth), n.kind, n.name, n.x, n.y, n.w, n.h);
        }
    }
    // One root frame (the copied board) at the origin, with its exact size.
    let roots: Vec<_> = plan.nodes.iter().filter(|n| n.parent.is_none()).collect();
    assert_eq!(roots.len(), 1);
    assert_eq!((roots[0].kind, roots[0].x, roots[0].y), (Kind::Frame, 0.0, 0.0));
    assert_eq!([roots[0].w, roots[0].h], plan.size);
    // Text from instances (overridden button labels) and plain frames.
    let texts: Vec<&str> = plan.nodes.iter().filter_map(|n| n.text.as_ref()).map(|t| t.characters.as_str()).collect();
    assert!(texts.contains(&"What are we creating today?"), "{texts:?}");
    assert!(texts.contains(&"New Project"), "instance text override: {texts:?}");
    assert!(plan.stats.images > 0 && plan.stats.rasters > 0);
    // Every image node has a URL; parents precede children.
    for (i, n) in plan.nodes.iter().enumerate() {
        if let Some(h) = &n.image {
            assert!(plan.images.contains_key(h));
        }
        assert!(n.parent.is_none_or(|p| p < i));
        assert!(n.w > 0.0 && n.h > 0.0);
    }
}
