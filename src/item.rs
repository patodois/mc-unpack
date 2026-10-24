//! Item models.
//!
//! - **`builtin/generated`** (the parent of nearly every flat item): the game builds, for each
//!   `layerN` texture, a one-pixel-thick slab: a front and a back face over the whole 16 x 16 square
//!   at z 7.5 to 8.5, plus a one-pixel-wide side strip on every edge where an opaque pixel borders a
//!   transparent one (or the texture's border). [`layer_elements`] re-creates those elements from
//!   the texture's alpha, and they go through the same baker as any other element. A pixel is
//!   transparent when its alpha is 0 in every frame of the texture.
//! - **`items/<name>.json` definitions** (1.21.4 and later) say which model an item uses, possibly
//!   depending on run-time state (use, time, count, a component). [`default_models`] walks one
//!   to the models of its **default branch** and lists what it could not follow.
//!
//! The one liberty taken: layers above the first are made thicker by [`LAYER_SEPARATION`] units on
//! each side so that coplanar layers do not z-fight in a viewer (the game draws them in order
//! instead).

use serde_json::Value;

use crate::model::{Dir, Element, Face};
use crate::texture::Texture;
use crate::tint::{color_from_json, ColormapKind, Tint};

/// Added to each side of a layer's slab per layer index, in model units (1/16 block)
pub const LAYER_SEPARATION: f64 = 0.005;

/// Opaque pixels of a texture over all its frames: `opaque[y * w + x]`, true where any frame has
/// alpha > 0
pub fn opacity_union(t: &Texture) -> (u32, u32, Vec<bool>) {
    let (w, h) = (t.frame_width, t.frame_height);
    let mut opaque = vec![false; (w * h) as usize];
    for i in 0..t.sheet_frames() {
        let f = t.frame(i).expect("frame in range");
        for y in 0..h {
            for x in 0..w {
                if f.alpha(x, y) != 0 {
                    opaque[(y * w + x) as usize] = true;
                }
            }
        }
    }
    (w, h, opaque)
}

fn face(dir_uv: [f64; 4], texture: &str, tint: u32) -> Face {
    Face {
        uv: Some(dir_uv),
        texture: texture.to_string(),
        cullface: None,
        rotation: 0,
        tintindex: Some(tint),
    }
}

/// The elements of one generated layer. `texture` is the reference its faces carry (`#layer0`).
pub fn layer_elements(layer: u32, texture: &str, w: u32, h: u32, opaque: &[bool]) -> Vec<Element> {
    let z0 = 7.5 - LAYER_SEPARATION * layer as f64;
    let z1 = 8.5 + LAYER_SEPARATION * layer as f64;
    let fx = 16.0 / w as f64;
    let fy = 16.0 / h as f64;
    let op = |x: i64, y: i64| -> bool {
        x >= 0
            && y >= 0
            && x < w as i64
            && y < h as i64
            && opaque[(y as usize) * w as usize + x as usize]
    };
    let mut out = Vec::new();
    out.push(Element {
        from: [0.0, 0.0, z0],
        to: [16.0, 16.0, z1],
        rotation: None,
        shade: true,
        light_emission: 0,
        faces: vec![
            (Dir::North, face([0.0, 0.0, 16.0, 16.0], texture, layer)),
            (Dir::South, face([0.0, 0.0, 16.0, 16.0], texture, layer)),
        ],
    });
    // Strips along rows (up and down edges)
    for y in 0..h as i64 {
        for (dir, dy) in [(Dir::Up, -1), (Dir::Down, 1)] {
            let mut x = 0i64;
            while x < w as i64 {
                if op(x, y) && !op(x, y + dy) {
                    let x0 = x;
                    while x < w as i64 && op(x, y) && !op(x, y + dy) {
                        x += 1;
                    }
                    let edge = if dir == Dir::Up {
                        16.0 - y as f64 * fy
                    } else {
                        16.0 - (y + 1) as f64 * fy
                    };
                    out.push(Element {
                        from: [x0 as f64 * fx, edge, z0],
                        to: [x as f64 * fx, edge, z1],
                        rotation: None,
                        shade: true,
                        light_emission: 0,
                        faces: vec![(
                            dir,
                            face(
                                [
                                    x0 as f64 * fx,
                                    y as f64 * fy,
                                    x as f64 * fx,
                                    (y + 1) as f64 * fy,
                                ],
                                texture,
                                layer,
                            ),
                        )],
                    });
                } else {
                    x += 1;
                }
            }
        }
    }
    // Strips along columns (left = west and right = east edges)
    for x in 0..w as i64 {
        for (dir, dx) in [(Dir::West, -1), (Dir::East, 1)] {
            let mut y = 0i64;
            while y < h as i64 {
                if op(x, y) && !op(x + dx, y) {
                    let y0 = y;
                    while y < h as i64 && op(x, y) && !op(x + dx, y) {
                        y += 1;
                    }
                    let edge = if dir == Dir::West {
                        x as f64 * fx
                    } else {
                        (x + 1) as f64 * fx
                    };
                    out.push(Element {
                        from: [edge, 16.0 - y as f64 * fy, z0],
                        to: [edge, 16.0 - y0 as f64 * fy, z1],
                        rotation: None,
                        shade: true,
                        light_emission: 0,
                        faces: vec![(
                            dir,
                            face(
                                [
                                    x as f64 * fx,
                                    y0 as f64 * fy,
                                    (x + 1) as f64 * fx,
                                    y as f64 * fy,
                                ],
                                texture,
                                layer,
                            ),
                        )],
                    });
                } else {
                    y += 1;
                }
            }
        }
    }
    out
}

/// A tint an item definition carries
#[derive(Clone, Debug, PartialEq)]
pub enum ItemTint {
    Constant([u8; 3]),
    Grass {
        temperature: f64,
        downfall: f64,
    },
    /// A colour the game computes from the stack (dye, potion, firework, map ...): the definition's
    /// `default`, which is what an unmodified item shows, when it gives one
    Dynamic {
        kind: String,
        default: Option<[u8; 3]>,
    },
}

impl ItemTint {
    /// The tint to apply: dynamic ones use their default if the data has one
    pub fn to_tint(&self) -> Option<Tint> {
        match self {
            ItemTint::Constant(c) => Some(Tint::Rgb(*c)),
            ItemTint::Grass {
                temperature,
                downfall,
            } => Some(Tint::Colormap {
                kind: ColormapKind::Grass,
                temperature: *temperature,
                downfall: *downfall,
            }),
            ItemTint::Dynamic { default, .. } => default.map(Tint::Rgb),
        }
    }
}

/// A model an item definition names
#[derive(Clone, Debug, PartialEq)]
pub struct ItemModelRef {
    /// `minecraft:item/diamond_sword`, `minecraft:block/stone`
    pub model: String,
    pub tints: Vec<ItemTint>,
}

/// What the default branch of an item definition comes to
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DefaultModels {
    pub models: Vec<ItemModelRef>,
    /// `special` models (chest, banner, head, shield ...): drawn by code, not data
    pub special: Vec<String>,
    /// Branches that could not be followed, with why
    pub notes: Vec<String>,
}

fn short(t: &str) -> &str {
    t.strip_prefix("minecraft:").unwrap_or(t)
}

fn parse_tints(v: Option<&Value>) -> Vec<ItemTint> {
    let Some(list) = v.and_then(|t| t.as_array()) else {
        return Vec::new();
    };
    list.iter()
        .map(|t| {
            let ty = t
                .get("type")
                .and_then(|s| s.as_str())
                .map(short)
                .unwrap_or("");
            match ty {
                "constant" => t
                    .get("value")
                    .and_then(color_from_json)
                    .map(ItemTint::Constant)
                    .unwrap_or(ItemTint::Dynamic {
                        kind: "constant (unreadable)".into(),
                        default: None,
                    }),
                "grass" => ItemTint::Grass {
                    temperature: t.get("temperature").and_then(|n| n.as_f64()).unwrap_or(0.5),
                    downfall: t.get("downfall").and_then(|n| n.as_f64()).unwrap_or(1.0),
                },
                other => ItemTint::Dynamic {
                    kind: other.to_string(),
                    default: t.get("default").and_then(color_from_json),
                },
            }
        })
        .collect()
}

/// Walks an item definition (`items/<name>.json`, the `model` object inside it) to the models of
/// its default branch:
///
/// - `model`: that model; `composite`: all of its models;
/// - `condition`: `on_false`; `select` and `range_dispatch`: `fallback`, else the first case or
///   entry (these depend on run-time state and the data does not say which is "default");
/// - `special`: listed in [`DefaultModels::special`], no geometry;
/// - `empty`, `bundle/selected_item`: nothing.
pub fn default_models(def: &Value) -> DefaultModels {
    let mut out = DefaultModels::default();
    let root = def.get("model").unwrap_or(def);
    walk(root, &mut out);
    out
}

fn walk(node: &Value, out: &mut DefaultModels) {
    let ty = node
        .get("type")
        .and_then(|s| s.as_str())
        .map(short)
        .unwrap_or("");
    match ty {
        "model" => match node.get("model").and_then(|m| m.as_str()) {
            Some(m) => out.models.push(ItemModelRef {
                model: m.to_string(),
                tints: parse_tints(node.get("tints")),
            }),
            None => out.notes.push("a `model` node without a model".into()),
        },
        "composite" => {
            if let Some(list) = node.get("models").and_then(|m| m.as_array()) {
                for m in list {
                    walk(m, out);
                }
            }
        }
        "condition" => {
            let branch = node.get("on_false").or_else(|| node.get("on_true"));
            match branch {
                Some(b) => {
                    out.notes.push(format!(
                        "condition on {}: took on_false",
                        node.get("property")
                            .and_then(|p| p.as_str())
                            .map(short)
                            .unwrap_or("?")
                    ));
                    walk(b, out)
                }
                None => out.notes.push("a `condition` without branches".into()),
            }
        }
        "select" | "range_dispatch" => {
            let prop = node
                .get("property")
                .and_then(|p| p.as_str())
                .map(short)
                .unwrap_or("?");
            let first = if ty == "select" {
                node.get("cases")
                    .and_then(|c| c.as_array())
                    .and_then(|c| c.first())
            } else {
                node.get("entries")
                    .and_then(|c| c.as_array())
                    .and_then(|c| c.first())
            }
            .and_then(|c| c.get("model"));
            match node.get("fallback").or(first) {
                Some(b) => {
                    out.notes.push(format!(
                        "{ty} on {prop}: took the {}",
                        if node.get("fallback").is_some() {
                            "fallback"
                        } else {
                            "first case"
                        }
                    ));
                    walk(b, out)
                }
                None => out
                    .notes
                    .push(format!("a `{ty}` without a fallback or cases")),
            }
        }
        "special" => {
            let kind = node
                .get("model")
                .and_then(|m| m.get("type"))
                .and_then(|s| s.as_str())
                .map(short)
                .unwrap_or("?");
            out.special.push(kind.to_string());
        }
        "empty" | "bundle/selected_item" => {}
        other => out.notes.push(format!("unknown item model type `{other}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bake::{bake_element, VariantRotation};
    use crate::jar::testjar::png;
    use crate::texture::decode_png;

    fn opaque_from(w: u32, h: u32, f: impl Fn(u32, u32) -> bool) -> Vec<bool> {
        let mut v = Vec::new();
        for y in 0..h {
            for x in 0..w {
                v.push(f(x, y));
            }
        }
        v
    }

    fn quads(els: &[Element]) -> Vec<crate::bake::Quad> {
        let mut out = Vec::new();
        for e in els {
            bake_element(
                e,
                VariantRotation::default(),
                &mut |_| Ok(("item/t".into(), false)),
                &mut out,
            )
            .unwrap();
        }
        out
    }

    #[test]
    fn a_full_square_has_front_back_and_four_edges() {
        let o = opaque_from(16, 16, |_, _| true);
        let els = layer_elements(0, "#layer0", 16, 16, &o);
        // front slab + one merged run per side
        assert_eq!(els.len(), 1 + 4);
        let q = quads(&els);
        assert_eq!(q.len(), 2 + 4);
        let faces: Vec<_> = q.iter().map(|q| q.face).collect();
        for d in [
            Dir::Up,
            Dir::Down,
            Dir::West,
            Dir::East,
            Dir::North,
            Dir::South,
        ] {
            assert!(faces.contains(&d), "{d:?}");
        }
        // the slab is one pixel thick, centred on the block
        let zs: Vec<f32> = q.iter().flat_map(|q| q.pos.iter().map(|p| p[2])).collect();
        assert!((zs.iter().cloned().fold(9.0, f32::min) - 7.5 / 16.0).abs() < 1e-6);
        assert!((zs.iter().cloned().fold(-9.0, f32::max) - 8.5 / 16.0).abs() < 1e-6);
    }

    #[test]
    fn one_opaque_pixel_has_four_one_pixel_edges() {
        let o = opaque_from(16, 16, |x, y| x == 5 && y == 3);
        let els = layer_elements(0, "#layer0", 16, 16, &o);
        let sides: Vec<_> = els.iter().skip(1).collect();
        assert_eq!(sides.len(), 4);
        // the UV of each strip is that pixel
        for s in &sides {
            let uv = s.faces[0].1.uv.unwrap();
            assert_eq!(
                [uv[0], uv[1], uv[2], uv[3]],
                [5.0, 3.0, 6.0, 4.0],
                "{:?}",
                s.faces[0].0
            );
        }
        // its left edge faces west at x = 5 and spans the pixel's height: y 12..13
        let west = sides.iter().find(|s| s.faces[0].0 == Dir::West).unwrap();
        assert_eq!(west.from, [5.0, 12.0, 7.5]);
        assert_eq!(west.to, [5.0, 13.0, 8.5]);
        let up = sides.iter().find(|s| s.faces[0].0 == Dir::Up).unwrap();
        assert_eq!(up.from[1], 13.0);
    }

    #[test]
    fn interior_pixels_have_no_edges_and_runs_merge() {
        // a 3 x 3 block of pixels inside the texture
        let o = opaque_from(16, 16, |x, y| (4..7).contains(&x) && (4..7).contains(&y));
        let els = layer_elements(0, "#layer0", 16, 16, &o);
        assert_eq!(els.len(), 1 + 4, "one merged run per side");
        let up = els
            .iter()
            .skip(1)
            .find(|s| s.faces[0].0 == Dir::Up)
            .unwrap();
        assert_eq!(up.from[0], 4.0);
        assert_eq!(up.to[0], 7.0);
    }

    #[test]
    fn a_hole_gets_inward_facing_edges() {
        // a ring: the hole's four neighbours each get an edge facing into the hole
        let o = opaque_from(16, 16, |x, y| {
            (4..9).contains(&x) && (4..9).contains(&y) && !(x == 6 && y == 6)
        });
        let els = layer_elements(0, "#layer0", 16, 16, &o);
        let dirs: Vec<Dir> = els.iter().skip(1).map(|e| e.faces[0].0).collect();
        // 4 outer runs + 4 hole edges
        assert_eq!(dirs.len(), 8);
    }

    #[test]
    fn empty_texture_has_only_front_and_back() {
        let o = opaque_from(16, 16, |_, _| false);
        let els = layer_elements(0, "#layer0", 16, 16, &o);
        assert_eq!(els.len(), 1);
    }

    #[test]
    fn layers_carry_their_tint_index_and_are_separated() {
        let o = opaque_from(16, 16, |_, _| true);
        let l0 = layer_elements(0, "#layer0", 16, 16, &o);
        let l2 = layer_elements(2, "#layer2", 16, 16, &o);
        assert_eq!(l2[0].faces[0].1.tintindex, Some(2));
        assert!(l2[0].from[2] < l0[0].from[2] && l2[0].to[2] > l0[0].to[2]);
    }

    #[test]
    fn a_texture_that_is_not_16_wide_scales_the_strips() {
        let o = opaque_from(32, 32, |x, y| x == 0 && y == 0);
        let els = layer_elements(0, "#layer0", 32, 32, &o);
        let west = els
            .iter()
            .skip(1)
            .find(|s| s.faces[0].0 == Dir::East)
            .unwrap();
        assert_eq!(west.from[0], 0.5);
        assert_eq!(west.to[1], 16.0);
    }

    #[test]
    fn opacity_is_the_union_of_frames() {
        // 2 x 4 sheet of two 2 x 2 frames: pixel (0,0) opaque only in frame 0, (1,1) only in frame 1
        let img = decode_png(&png(2, 4, |x, y| match (x, y) {
            (0, 0) => [1, 1, 1, 255],
            (1, 3) => [1, 1, 1, 255],
            _ => [0, 0, 0, 0],
        }))
        .unwrap();
        let meta = crate::texture::McMeta::parse(r#"{"animation":{}}"#).unwrap();
        let t = Texture::new(img, Some(&meta)).unwrap();
        let (w, h, o) = opacity_union(&t);
        assert_eq!((w, h), (2, 2));
        assert_eq!(o, vec![true, false, false, true]);
    }

    #[test]
    fn definition_walk() {
        let d: Value = serde_json::from_str(
            r##"{"model":{"type":"minecraft:composite","models":[
                {"type":"minecraft:model","model":"minecraft:item/a","tints":[{"type":"minecraft:constant","value":-12012264},{"type":"minecraft:dye","default":-6265536},{"type":"minecraft:potion"}]},
                {"type":"minecraft:select","property":"minecraft:display_context","cases":[{"model":{"type":"minecraft:model","model":"minecraft:item/c"},"when":"gui"}],
                 "fallback":{"type":"minecraft:model","model":"minecraft:item/b"}},
                {"type":"minecraft:condition","property":"minecraft:using_item","on_true":{"type":"minecraft:model","model":"minecraft:item/t"},"on_false":{"type":"minecraft:model","model":"minecraft:item/f"}},
                {"type":"minecraft:special","base":"minecraft:item/chest","model":{"type":"minecraft:chest","texture":"minecraft:normal"}},
                {"type":"minecraft:empty"}]}}"##,
        )
        .unwrap();
        let r = default_models(&d);
        let names: Vec<_> = r.models.iter().map(|m| m.model.as_str()).collect();
        assert_eq!(
            names,
            ["minecraft:item/a", "minecraft:item/b", "minecraft:item/f"]
        );
        assert_eq!(r.special, ["chest"]);
        assert_eq!(r.models[0].tints[0], ItemTint::Constant([0x48, 0xB5, 0x18]));
        assert_eq!(
            r.models[0].tints[1].to_tint(),
            Some(Tint::Rgb([0xA0, 0x65, 0x40]))
        );
        assert_eq!(r.models[0].tints[2].to_tint(), None);
        assert_eq!(r.notes.len(), 2);
    }

    #[test]
    fn grass_tint_becomes_a_colormap_lookup() {
        let d: Value = serde_json::from_str(
            r#"{"model":{"type":"minecraft:model","model":"minecraft:block/grass_block","tints":[{"type":"minecraft:grass","downfall":1.0,"temperature":0.5}]}}"#,
        )
        .unwrap();
        let r = default_models(&d);
        assert_eq!(
            r.models[0].tints[0].to_tint(),
            Some(Tint::Colormap {
                kind: ColormapKind::Grass,
                temperature: 0.5,
                downfall: 1.0
            })
        );
    }
}
