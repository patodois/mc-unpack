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

use anyhow::Result;

use crate::assets::Refused;
use crate::model::{Dir, Element, Face};
use crate::texture::Texture;
use crate::tint::{color_from_json, ColormapKind, Tint};

/// Largest generated layer texture, per side. Item textures are 16 x 16 (some 32); a 512 x 512 one
/// would be hundreds of thousands of strips.
pub const MAX_LAYER_SIDE: u32 = 128;

/// Most elements one generated layer may have (the front and back slab, and a strip per pixel
/// edge). The most any item in the eight jars needs is 259 (a diamond sword); this is 4 times that.
/// A texture whose alpha is a checkerboard has one strip per pixel edge and goes over it.
pub const MAX_LAYER_ELEMENTS: usize = 1_040;

/// Added to each side of a layer's slab per layer index, in model units (1/16 block)
pub const LAYER_SEPARATION: f64 = 0.005;

/// Opaque pixels of a texture over all its frames: `opaque[y * w + x]`, true where any frame has
/// alpha > 0
pub fn opacity_union(t: &Texture) -> (u32, u32, Vec<bool>) {
    let (w, h) = (t.frame_width, t.frame_height);
    let mut opaque = vec![false; (w * h) as usize];
    // only the frames the game shows: those the animation lists, or frame 0 of a static sprite
    for i in t.shown_frames() {
        let Ok(f) = t.frame(i) else { continue };
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
/// A layer over [`MAX_LAYER_SIDE`] or [`MAX_LAYER_ELEMENTS`] is refused (left out, with the reason).
pub fn layer_elements(
    layer: u32,
    texture: &str,
    w: u32,
    h: u32,
    opaque: &[bool],
) -> Result<Vec<Element>> {
    if w > MAX_LAYER_SIDE || h > MAX_LAYER_SIDE || opaque.len() != (w as usize) * (h as usize) {
        return Err(Refused(format!(
            "generated item layer {layer} is {w} x {h}; the limit is {MAX_LAYER_SIDE} x {MAX_LAYER_SIDE}"
        ))
        .into());
    }
    let too_many = || {
        Refused(format!(
            "generated item layer {layer} needs more than {MAX_LAYER_ELEMENTS} elements (one strip per edge of its alpha)"
        ))
    };
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
            // north faces are drawn mirrored in u relative to the south face, so that the same
            // texel sits behind the same texel at the same x (and meets the edge strips there)
            (Dir::North, face([16.0, 0.0, 0.0, 16.0], texture, layer)),
            (Dir::South, face([0.0, 0.0, 16.0, 16.0], texture, layer)),
        ],
    });
    // One strip per pixel edge, as the game makes them (no merging into runs). Each strip's UV is
    // its pixel shrunk by 0.1 texel on every side, so that filtering does not bleed the next pixel
    // in. The outward direction is used for the face: the game labels the left and right strips
    // with the opposite direction and draws them without culling, which changes only their shading.
    const INSET: f64 = 0.1;
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            if !op(x, y) {
                continue;
            }
            for (dir, dx, dy) in [
                (Dir::Up, 0, -1),
                (Dir::Down, 0, 1),
                (Dir::West, -1, 0),
                (Dir::East, 1, 0),
            ] {
                if op(x + dx, y + dy) {
                    continue;
                }
                if out.len() >= MAX_LAYER_ELEMENTS {
                    return Err(too_many().into());
                }
                let (x0, x1) = (x as f64 * fx, (x + 1) as f64 * fx);
                let (top, bottom) = (16.0 - y as f64 * fy, 16.0 - (y + 1) as f64 * fy);
                let (from, to) = match dir {
                    Dir::Up => ([x0, top, z0], [x1, top, z1]),
                    Dir::Down => ([x0, bottom, z0], [x1, bottom, z1]),
                    Dir::West => ([x0, bottom, z0], [x0, top, z1]),
                    _ => ([x1, bottom, z0], [x1, top, z1]),
                };
                let uv = [
                    (x as f64 + INSET) * fx,
                    (y as f64 + INSET) * fy,
                    (x as f64 + 1.0 - INSET) * fx,
                    (y as f64 + 1.0 - INSET) * fy,
                ];
                out.push(Element {
                    from,
                    to,
                    rotation: None,
                    shade: true,
                    light_emission: 0,
                    faces: vec![(dir, face(uv, texture, layer))],
                });
            }
        }
    }
    Ok(out)
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

/// Reads a definition's `tints`. Nothing is assumed: a tint whose data is missing or unreadable
/// becomes [`ItemTint::Dynamic`] with no default, and a line in `notes` says so.
fn parse_tints(v: Option<&Value>, model: &str, notes: &mut Vec<String>) -> Vec<ItemTint> {
    let Some(list) = v.and_then(|t| t.as_array()) else {
        return Vec::new();
    };
    list.iter()
        .enumerate()
        .map(|(i, t)| {
            let ty = t
                .get("type")
                .and_then(|s| s.as_str())
                .map(short)
                .unwrap_or("");
            let mut skip = |why: &str| {
                notes.push(format!(
                    "{model}: tint {i} ({ty}) {why}: left uncoloured"
                ));
                ItemTint::Dynamic {
                    kind: format!("{ty} ({why})"),
                    default: None,
                }
            };
            match ty {
                "constant" => match t.get("value").and_then(color_from_json) {
                    Some(c) => ItemTint::Constant(c),
                    None => skip("has no readable `value`"),
                },
                "grass" => match (
                    t.get("temperature").and_then(|n| n.as_f64()),
                    t.get("downfall").and_then(|n| n.as_f64()),
                ) {
                    (Some(temperature), Some(downfall)) => ItemTint::Grass {
                        temperature,
                        downfall,
                    },
                    _ => skip("needs `temperature` and `downfall`, and the data lacks one"),
                },
                other => {
                    let default = t.get("default").and_then(color_from_json);
                    if default.is_none() {
                        notes.push(format!(
                            "{model}: tint {i} ({other}) is computed by the game from the stack and has no `default`: left uncoloured"
                        ));
                    }
                    ItemTint::Dynamic {
                        kind: other.to_string(),
                        default,
                    }
                }
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
            Some(m) => {
                let tints = parse_tints(node.get("tints"), short(m), &mut out.notes);
                out.models.push(ItemModelRef {
                    model: m.to_string(),
                    tints,
                })
            }
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
                    let fallback = node.get("fallback").is_some();
                    let mut note = format!(
                        "{ty} on {prop}: took the {}",
                        if fallback { "fallback" } else { "first case" }
                    );
                    if fallback && prop == "display_context" {
                        // the cases name where the item is drawn as a sprite (gui, ground ...); the
                        // fallback is the model for the contexts they do not name (in the hand)
                        let named: Vec<String> = node
                            .get("cases")
                            .and_then(|c| c.as_array())
                            .into_iter()
                            .flatten()
                            .filter_map(|c| c.get("when"))
                            .map(|w| w.to_string())
                            .collect();
                        note.push_str(&format!(
                            " (the model for the display contexts the cases do not name; they name {})",
                            named.join(", ")
                        ));
                    }
                    out.notes.push(note);
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

    fn le(layer: u32, texture: &str, w: u32, h: u32, opaque: &[bool]) -> Vec<Element> {
        layer_elements(layer, texture, w, h, opaque).unwrap()
    }

    fn quads(els: &[Element]) -> Vec<crate::bake::Quad> {
        let mut out = Vec::new();
        for e in els {
            bake_element(
                e,
                VariantRotation::default(),
                &mut |_| Ok(("item/t".into(), false)),
                &mut out,
                &mut Vec::new(),
            )
            .unwrap();
        }
        out
    }

    #[test]
    fn a_full_square_has_front_back_and_four_edges() {
        let o = opaque_from(16, 16, |_, _| true);
        let els = le(0, "#layer0", 16, 16, &o);
        // the front slab, and a strip for every pixel edge on the border: 4 x 16
        assert_eq!(els.len(), 1 + 64);
        let q = quads(&els);
        assert_eq!(q.len(), 2 + 64);
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
        let els = le(0, "#layer0", 16, 16, &o);
        let sides: Vec<_> = els.iter().skip(1).collect();
        assert_eq!(sides.len(), 4);
        // the UV of each strip is that pixel, shrunk by a tenth of a texel on every side
        for s in &sides {
            let uv = s.faces[0].1.uv.unwrap();
            let want = [5.1, 3.1, 5.9, 3.9];
            assert!(
                uv.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-9),
                "{:?}: {uv:?}",
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
    fn interior_pixels_have_no_edges() {
        // a 3 x 3 block of pixels inside the texture
        let o = opaque_from(16, 16, |x, y| (4..7).contains(&x) && (4..7).contains(&y));
        let els = le(0, "#layer0", 16, 16, &o);
        // 4 sides of 3 pixels, one strip each, and none for the pixel in the middle
        assert_eq!(els.len(), 1 + 12);
        let ups: Vec<_> = els
            .iter()
            .skip(1)
            .filter(|s| s.faces[0].0 == Dir::Up)
            .collect();
        assert_eq!(ups.len(), 3);
        assert!(ups.iter().all(|u| (u.to[0] - u.from[0] - 1.0).abs() < 1e-9));
    }

    #[test]
    fn a_hole_gets_inward_facing_edges() {
        // a ring: the hole's four neighbours each get an edge facing into the hole
        let o = opaque_from(16, 16, |x, y| {
            (4..9).contains(&x) && (4..9).contains(&y) && !(x == 6 && y == 6)
        });
        let els = le(0, "#layer0", 16, 16, &o);
        let dirs: Vec<Dir> = els.iter().skip(1).map(|e| e.faces[0].0).collect();
        // 4 x 5 on the outside + 4 round the hole
        assert_eq!(dirs.len(), 24);
    }

    #[test]
    fn empty_texture_has_only_front_and_back() {
        let o = opaque_from(16, 16, |_, _| false);
        let els = le(0, "#layer0", 16, 16, &o);
        assert_eq!(els.len(), 1);
    }

    #[test]
    fn layers_carry_their_tint_index_and_are_separated() {
        let o = opaque_from(16, 16, |_, _| true);
        let l0 = le(0, "#layer0", 16, 16, &o);
        let l2 = le(2, "#layer2", 16, 16, &o);
        assert_eq!(l2[0].faces[0].1.tintindex, Some(2));
        assert!(l2[0].from[2] < l0[0].from[2] && l2[0].to[2] > l0[0].to[2]);
    }

    #[test]
    fn a_texture_that_is_not_16_wide_scales_the_strips() {
        let o = opaque_from(32, 32, |x, y| x == 0 && y == 0);
        let els = le(0, "#layer0", 32, 32, &o);
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
        assert_eq!(r.notes.len(), 3);
        assert!(
            r.notes
                .iter()
                .any(|n| n.contains("potion") && n.contains("no `default`")),
            "{:?}",
            r.notes
        );
    }

    #[test]
    fn a_display_context_select_takes_the_fallback_and_says_what_that_means() {
        let d: Value = serde_json::from_str(
            r#"{"model":{"type":"minecraft:select","property":"minecraft:display_context",
                "cases":[{"when":["gui","ground"],"model":{"type":"minecraft:model","model":"minecraft:item/sprite"}}],
                "fallback":{"type":"minecraft:model","model":"minecraft:item/in_hand"}}}"#,
        )
        .unwrap();
        let r = default_models(&d);
        assert_eq!(r.models[0].model, "minecraft:item/in_hand");
        assert!(
            r.notes[0].contains("display contexts the cases do not name"),
            "{:?}",
            r.notes
        );
        assert!(r.notes[0].contains("gui"), "{:?}", r.notes);
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

    #[test]
    fn front_and_back_faces_sample_the_same_texel_at_the_same_x() {
        // a texture that is opaque everywhere: the south (front) and north (back) quads and the
        // edge strips must agree on which u belongs to which x
        let o = opaque_from(16, 16, |_, _| true);
        let q = quads(&le(0, "#layer0", 16, 16, &o));
        let u_at = |face: Dir, x: f32| -> Vec<f32> {
            let mut v = Vec::new();
            for quad in q.iter().filter(|q| q.face == face) {
                for i in 0..4 {
                    if (quad.pos[i][0] - x).abs() < 1e-6 && (quad.pos[i][1] - 1.0).abs() < 1e-6 {
                        v.push(quad.uv[i][0]);
                    }
                }
            }
            v
        };
        for x in [0.0f32, 1.0] {
            let s = u_at(Dir::South, x);
            let n = u_at(Dir::North, x);
            assert!(!s.is_empty() && !n.is_empty());
            assert!((s[0] - x).abs() < 1e-6, "south u at x={x}: {s:?}");
            assert!(
                (n[0] - x).abs() < 1e-6,
                "north u at x={x}: {n:?} (was mirrored)"
            );
        }
    }

    #[test]
    fn oversized_layers_are_refused() {
        let big = opaque_from(129, 4, |_, _| true);
        assert!(layer_elements(0, "#layer0", 129, 4, &big).is_err());
        let checker = opaque_from(64, 64, |x, y| (x + y) % 2 == 0);
        let e = layer_elements(0, "#layer0", 64, 64, &checker).unwrap_err();
        assert!(
            e.downcast_ref::<crate::assets::Refused>().is_some(),
            "{e:#}"
        );
        assert!(layer_elements(
            0,
            "#layer0",
            16,
            16,
            &opaque_from(16, 16, |x, y| (x + y) % 2 == 0)
        )
        .is_ok());
    }

    #[test]
    fn tints_with_missing_data_are_noted_not_defaulted() {
        let d: Value = serde_json::from_str(
            r#"{"model":{"type":"minecraft:model","model":"minecraft:item/a","tints":[
                {"type":"minecraft:grass"},
                {"type":"minecraft:grass","temperature":0.2},
                {"type":"minecraft:constant"},
                {"type":"minecraft:dye"}]}}"#,
        )
        .unwrap();
        let r = default_models(&d);
        let t = &r.models[0].tints;
        assert_eq!(t.len(), 4);
        assert!(t.iter().all(|t| t.to_tint().is_none()), "{t:?}");
        assert_eq!(r.notes.len(), 4, "{:?}", r.notes);
        assert!(r.notes[0].contains("temperature") && r.notes[0].contains("downfall"));
        assert!(r.notes[2].contains("value"));
    }

    #[test]
    fn opacity_comes_only_from_the_frames_the_game_shows() {
        // 2 x 6 sheet of three 2 x 2 frames; pixel (0,0) opaque in frame 0, (1,1) in frame 1, (0,1) in frame 2
        let img = decode_png(&png(2, 6, |x, y| match (x, y) {
            (0, 0) => [1, 1, 1, 255],
            (1, 3) => [1, 1, 1, 255],
            (0, 5) => [1, 1, 1, 255],
            _ => [0, 0, 0, 0],
        }))
        .unwrap();
        let with = |meta: &str| {
            let m = crate::texture::McMeta::parse(meta).unwrap();
            let t = Texture::new(img.clone(), Some(&m)).unwrap();
            opacity_union(&t).2
        };
        // frames 1 and 2 listed: frame 0's pixel does not count
        assert_eq!(
            with(r#"{"animation":{"frames":[1,2]}}"#),
            vec![false, false, true, true]
        );
        // frame 0 only (one valid frame left, so the sprite is static): just its pixel
        assert_eq!(
            with(r#"{"animation":{"frames":[2,9]}}"#),
            vec![true, false, false, false]
        );
    }
}
