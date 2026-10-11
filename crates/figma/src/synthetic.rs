//! A synthetic Figma clipboard for tests: a small kiwi schema shaped like Figma's (same
//! field names), a scene encoded with it, compressed and wrapped like the real HTML payload.

use base64::Engine;
use serde_json::{Value as J, json};

use crate::kiwi::tests::Writer;

/// `(name, kind, fields)`; a field is `(name, type, array, id)` where type is a builtin
/// (`bool byte int uint float string`) or a definition name.
type DefSpec = (&'static str, u8, Vec<(&'static str, &'static str, bool)>);

fn defs() -> Vec<DefSpec> {
    let f = |n, t| (n, t, false);
    let a = |n, t| (n, t, true);
    vec![
        ("NodeType", 0, vec![f("DOCUMENT", ""), f("CANVAS", ""), f("FRAME", ""), f("GROUP", ""), f("TEXT", ""), f("RECTANGLE", ""), f("VECTOR", ""), f("INSTANCE", ""), f("SYMBOL", ""), f("ELLIPSE", ""), f("VARIABLE", ""), f("VARIABLE_SET", "")]),
        ("PaintType", 0, vec![f("SOLID", ""), f("IMAGE", ""), f("GRADIENT_LINEAR", "")]),
        ("ScaleMode", 0, vec![f("FILL", ""), f("FIT", ""), f("STRETCH", "")]),
        ("PropField", 0, vec![f("VISIBLE", ""), f("TEXT_DATA", "")]),
        ("GUID", 1, vec![f("sessionID", "uint"), f("localID", "uint")]),
        ("Vector", 1, vec![f("x", "float"), f("y", "float")]),
        ("Color", 1, vec![f("r", "float"), f("g", "float"), f("b", "float"), f("a", "float")]),
        ("Matrix", 1, vec![f("m00", "float"), f("m01", "float"), f("m02", "float"), f("m10", "float"), f("m11", "float"), f("m12", "float")]),
        ("ParentIndex", 2, vec![f("guid", "GUID"), f("position", "string")]),
        ("Image", 2, vec![a("hash", "byte"), f("name", "string")]),
        ("Stop", 2, vec![f("color", "Color"), f("position", "float")]),
        ("Alias", 2, vec![f("guid", "GUID")]),
        ("VarRefValue", 2, vec![f("alias", "Alias")]),
        ("VarRef", 2, vec![f("value", "VarRefValue")]),
        ("Mode", 2, vec![f("id", "GUID"), f("name", "string")]),
        ("ColorValue", 2, vec![f("colorValue", "Color")]),
        ("ColorData", 2, vec![f("value", "ColorValue")]),
        ("ModeValue", 2, vec![f("modeID", "GUID"), f("variableData", "ColorData")]),
        ("ModeValues", 2, vec![a("entries", "ModeValue")]),
        (
            "Paint",
            2,
            vec![
                f("type", "PaintType"),
                f("color", "Color"),
                f("opacity", "float"),
                f("visible", "bool"),
                f("image", "Image"),
                f("imageScaleMode", "ScaleMode"),
                f("transform", "Matrix"),
                a("stops", "Stop"),
                f("originalImageWidth", "float"),
                f("originalImageHeight", "float"),
                f("colorVar", "VarRef"),
            ],
        ),
        ("TextData", 2, vec![f("characters", "string")]),
        ("FontName", 2, vec![f("family", "string"), f("style", "string")]),
        ("VectorData", 2, vec![f("vectorNetworkBlob", "uint"), f("normalizedSize", "Vector")]),
        ("GuidPath", 2, vec![a("guids", "GUID")]),
        ("SymbolData", 2, vec![f("symbolID", "GUID"), a("symbolOverrides", "NodeChange")]),
        ("VarValue", 2, vec![f("boolValue", "bool"), f("textDataValue", "TextData")]),
        ("VarData", 2, vec![f("value", "VarValue")]),
        ("PropAssignment", 2, vec![f("defID", "GUID"), f("varValue", "VarData")]),
        ("PropRef", 2, vec![f("defID", "GUID"), f("componentPropNodeField", "PropField")]),
        (
            "NodeChange",
            2,
            vec![
                f("guid", "GUID"),
                f("parentIndex", "ParentIndex"),
                f("type", "NodeType"),
                f("name", "string"),
                f("visible", "bool"),
                f("opacity", "float"),
                f("size", "Vector"),
                f("transform", "Matrix"),
                a("fillPaints", "Paint"),
                a("strokePaints", "Paint"),
                f("strokeWeight", "float"),
                f("cornerRadius", "float"),
                f("textData", "TextData"),
                f("fontSize", "float"),
                f("fontName", "FontName"),
                f("vectorData", "VectorData"),
                f("symbolData", "SymbolData"),
                a("derivedSymbolData", "NodeChange"),
                f("guidPath", "GuidPath"),
                f("overrideKey", "GUID"),
                a("componentPropAssignments", "PropAssignment"),
                a("componentPropRefs", "PropRef"),
                f("internalOnly", "bool"),
                f("resizeToFit", "bool"),
                a("variableSetModes", "Mode"),
                f("variableSetID", "Alias"),
                f("variableDataValues", "ModeValues"),
            ],
        ),
        ("Blob", 2, vec![a("bytes", "byte")]),
        ("Message", 2, vec![a("nodeChanges", "NodeChange"), a("blobs", "Blob")]),
    ]
}

const BUILTIN: [&str; 6] = ["bool", "byte", "int", "uint", "float", "string"];

fn type_code(defs: &[DefSpec], t: &str) -> i32 {
    match BUILTIN.iter().position(|b| *b == t) {
        Some(i) => -(i as i32) - 1,
        None => defs.iter().position(|d| d.0 == t).unwrap_or_else(|| panic!("no type {t}")) as i32,
    }
}

fn encode_schema(defs: &[DefSpec]) -> Vec<u8> {
    let mut w = Writer::default();
    w.uint(defs.len() as u32);
    for (name, kind, fields) in defs {
        w.str(name).byte(*kind).uint(fields.len() as u32);
        for (i, (fname, t, arr)) in fields.iter().enumerate() {
            let code = if *kind == 0 { -4 } else { type_code(defs, t) };
            let id = if *kind == 0 { i as u32 } else { i as u32 + 1 };
            w.str(fname).int(code).byte(*arr as u8).uint(id);
        }
    }
    w.0
}

fn encode_value(defs: &[DefSpec], t: &str, v: &J, w: &mut Writer) {
    match t {
        "bool" => {
            w.byte(v.as_bool().unwrap() as u8);
        }
        "byte" => {
            w.byte(v.as_u64().unwrap() as u8);
        }
        "uint" => {
            w.uint(v.as_u64().unwrap() as u32);
        }
        "int" => {
            w.int(v.as_i64().unwrap() as i32);
        }
        "float" => {
            w.float(v.as_f64().unwrap() as f32);
        }
        "string" => {
            w.str(v.as_str().unwrap());
        }
        _ => {
            let (_, kind, fields) = defs.iter().find(|d| d.0 == t).unwrap();
            match kind {
                0 => {
                    let i = fields.iter().position(|f| f.0 == v.as_str().unwrap()).unwrap_or_else(|| panic!("{t} has no {v}"));
                    w.uint(i as u32);
                }
                1 => {
                    for (name, ft, _) in fields {
                        encode_value(defs, ft, &v[*name], w);
                    }
                }
                _ => {
                    for (i, (name, ft, arr)) in fields.iter().enumerate() {
                        let Some(fv) = v.get(*name) else { continue };
                        w.uint(i as u32 + 1);
                        if *arr {
                            let items = fv.as_array().unwrap();
                            w.uint(items.len() as u32);
                            for it in items {
                                encode_value(defs, ft, it, w);
                            }
                        } else {
                            encode_value(defs, ft, fv, w);
                        }
                    }
                    w.uint(0);
                }
            }
        }
    }
}

fn g(s: u32, l: u32) -> J {
    json!({"sessionID": s, "localID": l})
}

fn at(x: f64, y: f64) -> J {
    json!({"m00": 1, "m01": 0, "m02": x, "m10": 0, "m11": 1, "m12": y})
}

fn solid(r: f64, gg: f64, b: f64) -> J {
    json!({"type": "SOLID", "color": {"r": r, "g": gg, "b": b, "a": 1}, "opacity": 1, "visible": true})
}

pub const IMAGE_HASH: &str = "00112233445566778899aabbccddeeff00112233";

/// Triangle vector network (normalized 10×10).
fn triangle() -> Vec<u8> {
    let mut b = Vec::new();
    for n in [3u32, 3, 1] {
        b.extend_from_slice(&n.to_le_bytes());
    }
    for (x, y) in [(0f32, 0f32), (10.0, 0.0), (5.0, 10.0)] {
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&x.to_le_bytes());
        b.extend_from_slice(&y.to_le_bytes());
    }
    for (s, e) in [(0u32, 1u32), (1, 2), (2, 0)] {
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&s.to_le_bytes());
        b.extend_from_slice(&[0; 8]);
        b.extend_from_slice(&e.to_le_bytes());
        b.extend_from_slice(&[0; 8]);
    }
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(&3u32.to_le_bytes());
    for s in [0u32, 1, 2] {
        b.extend_from_slice(&s.to_le_bytes());
    }
    b
}

/// The scene: a white rounded board with a title, an image, a vector, a nested group of vector
/// art, a hidden layer, and an instance whose label comes from a component property.
pub fn scene() -> J {
    let hash: Vec<u8> = (0..20).map(|i| u8::from_str_radix(&IMAGE_HASH[i * 2..i * 2 + 2], 16).unwrap()).collect();
    let pi = |p: J, pos: &str| json!({"guid": p, "position": pos});
    json!({
        "nodeChanges": [
            {"guid": g(0, 0), "type": "DOCUMENT", "name": "Document"},
            {"guid": g(0, 1), "parentIndex": pi(g(0, 0), "!"), "type": "CANVAS", "name": "Page 1"},
            {"guid": g(9, 1), "parentIndex": pi(g(0, 0), "~"), "type": "CANVAS", "name": "Internal Only Canvas", "internalOnly": true, "visible": false},
            // Component: a 120×40 button with a label bound to prop 5:1.
            {"guid": g(9, 2), "parentIndex": pi(g(9, 1), "!"), "type": "SYMBOL", "name": "Button", "size": {"x": 120, "y": 40}, "transform": at(0.0, 0.0),
             "fillPaints": [solid(0.0, 0.0, 1.0)], "cornerRadius": 6},
            {"guid": g(9, 3), "parentIndex": pi(g(9, 2), "!"), "type": "TEXT", "name": "Label", "overrideKey": g(6, 1), "size": {"x": 60, "y": 16}, "transform": at(10.0, 12.0),
             "fillPaints": [solid(1.0, 1.0, 1.0)], "textData": {"characters": "Button"}, "fontSize": 14, "fontName": {"family": "Inter", "style": "Semi Bold"},
             "componentPropRefs": [{"defID": g(5, 1), "componentPropNodeField": "TEXT_DATA"}]},
            // The board.
            {"guid": g(1, 1), "parentIndex": pi(g(0, 1), "!"), "type": "FRAME", "name": "Board", "size": {"x": 400, "y": 300}, "transform": at(1000.0, 2000.0),
             "fillPaints": [solid(1.0, 1.0, 1.0)], "cornerRadius": 12},
            {"guid": g(1, 2), "parentIndex": pi(g(1, 1), "!"), "type": "TEXT", "name": "Title", "size": {"x": 200, "y": 30}, "transform": at(20.0, 20.0),
             "fillPaints": [solid(0.0, 0.0, 0.0)], "textData": {"characters": "Hello Figma"}, "fontSize": 24, "fontName": {"family": "Inter", "style": "Bold"}},
            {"guid": g(1, 3), "parentIndex": pi(g(1, 1), "\""), "type": "RECTANGLE", "name": "Photo", "size": {"x": 160, "y": 90}, "transform": at(20.0, 60.0), "cornerRadius": 8,
             "fillPaints": [{"type": "IMAGE", "opacity": 1, "visible": true, "image": {"hash": hash, "name": "photo"}, "imageScaleMode": "FILL", "originalImageWidth": 1600, "originalImageHeight": 900}]},
            {"guid": g(1, 4), "parentIndex": pi(g(1, 1), "#"), "type": "VECTOR", "name": "Arrow", "size": {"x": 20, "y": 20}, "transform": at(200.0, 60.0),
             "fillPaints": [solid(1.0, 0.0, 0.0)], "vectorData": {"vectorNetworkBlob": 0, "normalizedSize": {"x": 10, "y": 10}}},
            {"guid": g(1, 5), "parentIndex": pi(g(1, 1), "$"), "type": "FRAME", "name": "Icons", "resizeToFit": true, "size": {"x": 50, "y": 20}, "transform": at(250.0, 60.0)},
            {"guid": g(1, 6), "parentIndex": pi(g(1, 5), "!"), "type": "ELLIPSE", "name": "Dot", "size": {"x": 20, "y": 20}, "transform": at(0.0, 0.0), "fillPaints": [solid(0.0, 1.0, 0.0)]},
            {"guid": g(1, 7), "parentIndex": pi(g(1, 5), "\""), "type": "ELLIPSE", "name": "Dot", "size": {"x": 20, "y": 20}, "transform": at(30.0, 0.0), "fillPaints": [solid(0.0, 1.0, 0.0)]},
            {"guid": g(1, 8), "parentIndex": pi(g(1, 1), "%"), "type": "RECTANGLE", "name": "Hidden", "visible": false, "size": {"x": 10, "y": 10}, "transform": at(0.0, 0.0), "fillPaints": [solid(0.0, 0.0, 0.0)]},
            {"guid": g(1, 9), "parentIndex": pi(g(1, 1), "&"), "type": "INSTANCE", "name": "Button", "size": {"x": 140, "y": 40}, "transform": at(20.0, 200.0),
             "fillPaints": [solid(0.0, 0.0, 1.0)], "cornerRadius": 6,
             "symbolData": {"symbolID": g(9, 2), "symbolOverrides": []},
             "derivedSymbolData": [{"guidPath": {"guids": [g(6, 1)]}, "size": {"x": 80, "y": 16}}],
             "componentPropAssignments": [{"defID": g(5, 1), "varValue": {"value": {"textDataValue": {"characters": "Submit"}}}}]},
        ],
        "blobs": [{"bytes": triangle()}],
    })
}

/// A dark board whose paints are bound to a two-mode color variable; one cached color is
/// stale (from the light mode).
pub fn themed_scene() -> J {
    let pi = |p: J, pos: &str| json!({"guid": p, "position": pos});
    let rgb = |v: f64| json!({"r": v, "g": v, "b": v, "a": 1});
    let bound = |cached: f64| json!({"type": "SOLID", "color": rgb(cached), "opacity": 1, "visible": true, "colorVar": {"value": {"alias": {"guid": g(3, 1)}}}});
    let rect = |l: u32, pos: &str, cached: f64| json!({"guid": g(1, l), "parentIndex": pi(g(1, 1), pos), "type": "FRAME", "name": format!("Card {l}"), "size": {"x": 10, "y": 10}, "transform": at(0.0, 0.0), "fillPaints": [bound(cached)]});
    json!({
        "nodeChanges": [
            {"guid": g(0, 0), "type": "DOCUMENT", "name": "Document"},
            {"guid": g(0, 1), "parentIndex": pi(g(0, 0), "!"), "type": "CANVAS", "name": "Page 1"},
            {"guid": g(9, 1), "parentIndex": pi(g(0, 0), "~"), "type": "CANVAS", "name": "Internal Only Canvas", "internalOnly": true},
            {"guid": g(3, 0), "parentIndex": pi(g(9, 1), "!"), "type": "VARIABLE_SET", "name": "Tokens", "variableSetModes": [{"id": g(4, 1), "name": "light"}, {"id": g(4, 2), "name": "dark"}]},
            {"guid": g(3, 1), "parentIndex": pi(g(9, 1), "\""), "type": "VARIABLE", "name": "surface", "variableSetID": {"guid": g(3, 0)},
             "variableDataValues": {"entries": [{"modeID": g(4, 1), "variableData": {"value": {"colorValue": rgb(1.0)}}}, {"modeID": g(4, 2), "variableData": {"value": {"colorValue": rgb(0.0)}}}]}},
            {"guid": g(1, 1), "parentIndex": pi(g(0, 1), "!"), "type": "FRAME", "name": "Board", "size": {"x": 100, "y": 100}, "transform": at(0.0, 0.0)},
            rect(2, "!", 0.0), rect(3, "\"", 0.0), rect(4, "#", 1.0),
        ],
        "blobs": [],
    })
}

/// The scene as Figma clipboard HTML.
pub fn html() -> String {
    html_of(&scene())
}

pub fn html_of(scene: &J) -> String {
    let defs = defs();
    let schema = encode_schema(&defs);
    let mut msg = Writer::default();
    encode_value(&defs, "Message", scene, &mut msg);
    let mut archive = b"fig-kiwi".to_vec();
    archive.extend_from_slice(&15u32.to_le_bytes());
    for chunk in [miniz_oxide::deflate::compress_to_vec(&schema, 6), miniz_oxide::deflate::compress_to_vec_zlib(&msg.0, 6)] {
        archive.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        archive.extend_from_slice(&chunk);
    }
    let meta = json!({"fileKey": "synthetic", "pasteID": 1, "dataType": "scene", "imageHashes": {IMAGE_HASH: "https://example.com/img/photo.png"}});
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    format!(
        r#"<meta charset="utf-8"><span data-metadata="<!--(figmeta){}(/figmeta)-->"></span><span data-buffer="<!--(figma){}(/figma)-->"></span><span>Hello Figma</span>"#,
        b64(meta.to_string().as_bytes()),
        b64(&archive)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{Kind, Op, Paint};

    #[test]
    fn synthetic_board_maps_to_nodes() {
        let html = html();
        assert!(crate::is_figma_html(&html));
        let plan = crate::plan_html(&html).unwrap();
        let by = |name: &str| plan.nodes.iter().position(|n| n.name == name).unwrap_or_else(|| panic!("no {name}: {:#?}", plan.nodes));
        assert_eq!(plan.size, [400.0, 300.0]);
        assert_eq!(plan.file_key.as_deref(), Some("synthetic"));

        let board = &plan.nodes[by("Board")];
        assert_eq!((board.kind, board.parent, board.x, board.y, board.w, board.h), (Kind::Frame, None, 0.0, 0.0, 400.0, 300.0));
        assert_eq!(board.fill.as_deref(), Some("#ffffff"));
        assert_eq!(board.radius, Some(12.0));

        let title = &plan.nodes[by("Title")];
        let t = title.text.as_ref().unwrap();
        assert_eq!((title.kind, title.x, title.y, title.w, title.h), (Kind::Text, 20.0, 20.0, 200.0, 30.0));
        assert_eq!((t.characters.as_str(), t.font_size, t.font_weight, t.color.as_deref()), ("Hello Figma", 24.0, 700, Some("#000000")));

        let photo = &plan.nodes[by("Photo")];
        assert_eq!((photo.kind, photo.image.as_deref(), photo.radius), (Kind::Image, Some(IMAGE_HASH), Some(8.0)));
        assert_eq!(plan.images[IMAGE_HASH], "https://example.com/img/photo.png");

        let arrow = &plan.nodes[by("Arrow")];
        assert_eq!((arrow.kind, arrow.x, arrow.y, arrow.w, arrow.h), (Kind::Raster, 200.0, 60.0, 20.0, 20.0));
        let ops = &arrow.raster.as_ref().unwrap().ops;
        match &ops[0] {
            Op::Fill { path, paint: Paint::Solid { color, .. }, m, .. } => {
                assert_eq!(path, "M 0 0 L 20 0 L 10 20 L 0 0 Z");
                assert_eq!(color, "#ff0000");
                assert_eq!(m, &[1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
            }
            o => panic!("{o:?}"),
        }

        // A group of only vector art becomes one raster with both dots.
        let icons = &plan.nodes[by("Icons")];
        assert_eq!((icons.kind, icons.w, icons.h), (Kind::Raster, 50.0, 20.0));
        assert_eq!(icons.raster.as_ref().unwrap().ops.len(), 2);

        assert!(!plan.nodes.iter().any(|n| n.name == "Hidden"));

        // Instance: expanded from the component, label from the property, size from derived data.
        let button = by("Button");
        assert_eq!((plan.nodes[button].kind, plan.nodes[button].w), (Kind::Frame, 140.0));
        assert_eq!(plan.nodes[button].fill.as_deref(), Some("#0000ff"));
        let label = &plan.nodes[by("Label")];
        assert_eq!(label.parent, Some(button));
        assert_eq!((label.x, label.y, label.w, label.h), (10.0, 12.0, 80.0, 16.0));
        assert_eq!(label.text.as_ref().unwrap().characters, "Submit");
        assert_eq!(label.text.as_ref().unwrap().font_weight, 600);

        // Ops serialize with camelCase fields (the client reads `clipM`).
        let json = serde_json::to_string(&plan).unwrap();
        assert!(!json.contains("clip_m"));

        // Parents first, z-order bottom to top under the board.
        let kids: Vec<&str> = plan.nodes.iter().filter(|n| n.parent == Some(0)).map(|n| n.name.as_str()).collect();
        assert_eq!(kids, ["Title", "Photo", "Arrow", "Icons", "Button"]);
    }

    #[test]
    fn variable_mode_is_inferred_from_cached_colors() {
        let plan = crate::plan_html(&html_of(&themed_scene())).unwrap();
        let fills: Vec<&str> = plan.nodes.iter().filter(|n| n.name.starts_with("Card")).map(|n| n.fill.as_deref().unwrap()).collect();
        // Two cached colors say "dark", so the stale light one is re-resolved too.
        assert_eq!(fills, ["#000000", "#000000", "#000000"]);
    }
}
