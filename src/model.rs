//! `assets/minecraft/models/**.json`: parsing, parent inheritance and texture variables.
//!
//! A model file may have a `parent` (another model file, or one of the game's built-in roots
//! `builtin/generated`, `builtin/entity`), `textures` (names to texture paths, or to `#other` names),
//! and `elements` (boxes with per-face texture, UV, rotation, cull face and tint index). A child
//! inherits the parent's textures (its own override by name) and its elements (replaced, not merged,
//! when the child has any). [`resolve`] walks the chain; [`ResolvedModel::texture`] follows `#name`
//! references to a texture path.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Dir {
    Down,
    Up,
    North,
    South,
    West,
    East,
}

impl Dir {
    pub const ALL: [Dir; 6] = [
        Dir::Down,
        Dir::Up,
        Dir::North,
        Dir::South,
        Dir::West,
        Dir::East,
    ];

    pub fn parse(s: &str) -> Option<Dir> {
        match s {
            "down" => Some(Dir::Down),
            "up" => Some(Dir::Up),
            "north" => Some(Dir::North),
            "south" => Some(Dir::South),
            "west" => Some(Dir::West),
            "east" => Some(Dir::East),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Dir::Down => "down",
            Dir::Up => "up",
            Dir::North => "north",
            Dir::South => "south",
            Dir::West => "west",
            Dir::East => "east",
        }
    }

    /// Unit vector in model space: +X east, +Y up, +Z south
    pub fn normal(self) -> [i32; 3] {
        match self {
            Dir::Down => [0, -1, 0],
            Dir::Up => [0, 1, 0],
            Dir::North => [0, 0, -1],
            Dir::South => [0, 0, 1],
            Dir::West => [-1, 0, 0],
            Dir::East => [1, 0, 0],
        }
    }

    pub fn from_normal(n: [i32; 3]) -> Option<Dir> {
        Dir::ALL.into_iter().find(|d| d.normal() == n)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
    Z,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ElementRotation {
    /// `{origin, axis, angle, rescale}`: a turn about one axis through `origin`; with `rescale` each
    /// axis is stretched by 1 / (the largest component of where that axis points after the turn), which
    /// is 1 / cos(angle) up to 45 degrees, so the element keeps its extent
    Axis {
        origin: [f64; 3],
        axis: Axis,
        angle: f64,
        rescale: bool,
    },
    /// `{origin, x, y, z}` (26.x): three angles in degrees (a missing one is 0), composed so that a
    /// point is turned about X first, then Y, then Z: the matrix `Rz * Ry * Rx`. `rescale` works as
    /// for the axis form
    Euler {
        origin: [f64; 3],
        x: f64,
        y: f64,
        z: f64,
        rescale: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Face {
    /// `[u0, v0, u1, v1]` in 0..16, or None for the default derived from the element's extent
    pub uv: Option<[f64; 4]>,
    /// `#name` or a texture path
    pub texture: String,
    pub cullface: Option<Dir>,
    /// 0, 90, 180 or 270
    pub rotation: i32,
    pub tintindex: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    pub from: [f64; 3],
    pub to: [f64; 3],
    pub rotation: Option<ElementRotation>,
    pub shade: bool,
    pub light_emission: u8,
    pub faces: Vec<(Dir, Face)>,
}

/// A value in `textures`
#[derive(Clone, Debug, PartialEq)]
pub struct TexEntry {
    /// `#name` or a texture path (`block/stone`, `minecraft:block/stone`, `blocks/stone` before 1.13)
    pub value: String,
    /// 26.x writes `{"sprite": ..., "force_translucent": true}`: the one place the data names a
    /// render layer
    pub force_translucent: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawModel {
    pub parent: Option<String>,
    pub textures: Vec<(String, TexEntry)>,
    pub elements: Option<Vec<Element>>,
    pub ambient_occlusion: Option<bool>,
    pub gui_light: Option<String>,
    /// The `display` section as written (transforms for the GUI, hand, ground ...); not used for
    /// the block geometry, kept for callers that want it
    pub display: Option<Value>,
}

fn num_array<const N: usize>(v: &Value, what: &str) -> Result<[f64; N]> {
    let a = v
        .as_array()
        .filter(|a| a.len() == N)
        .ok_or_else(|| anyhow!("{what}: expected a list of {N} numbers"))?;
    let mut out = [0.0; N];
    for (i, x) in a.iter().enumerate() {
        let n = x
            .as_f64()
            .ok_or_else(|| anyhow!("{what}: entry {i} is not a number"))?;
        if !n.is_finite() {
            bail!("{what}: entry {i} is not finite");
        }
        out[i] = n;
    }
    Ok(out)
}

/// The range the game accepts for an element's `from` and `to`
pub const POSITION_RANGE: (f64, f64) = (-16.0, 32.0);

fn position(v: &Value, what: &str) -> Result<[f64; 3]> {
    let p = num_array::<3>(v, what)?;
    if p.iter()
        .any(|c| *c < POSITION_RANGE.0 || *c > POSITION_RANGE.1)
    {
        bail!(
            "{what} {p:?} is outside the {} to {} the game accepts",
            POSITION_RANGE.0,
            POSITION_RANGE.1
        );
    }
    Ok(p)
}

/// A whole number written as `90` or `90.0` (the game reads both); anything else is an error,
/// never a silent 0
fn whole(v: &Value, what: &str) -> Result<i64> {
    if let Some(i) = v.as_i64() {
        return Ok(i);
    }
    match v.as_f64() {
        Some(f) if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e12 => Ok(f as i64),
        _ => bail!("{what} is not a whole number"),
    }
}

fn opt_bool(o: &serde_json::Map<String, Value>, key: &str) -> Result<Option<bool>> {
    match o.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => bail!("`{key}` is not true or false"),
    }
}

impl RawModel {
    pub fn parse(text: impl AsRef<[u8]>) -> Result<RawModel> {
        let v: Value = crate::jar::parse_json(text.as_ref()).context("model is not JSON")?;
        let Some(o) = v.as_object() else {
            bail!("model is not an object");
        };
        let parent = match o.get("parent") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => bail!("`parent` is not a string"),
        };
        let mut textures = Vec::new();
        if let Some(t) = o.get("textures") {
            let Some(m) = t.as_object() else {
                bail!("`textures` is not an object");
            };
            for (k, val) in m {
                let entry = match val {
                    Value::String(s) => TexEntry {
                        value: s.clone(),
                        force_translucent: false,
                    },
                    Value::Object(ob) => TexEntry {
                        value: ob
                            .get("sprite")
                            .and_then(|s| s.as_str())
                            .ok_or_else(|| anyhow!("texture {k:?}: object without a `sprite`"))?
                            .to_string(),
                        force_translucent: ob
                            .get("force_translucent")
                            .and_then(|b| b.as_bool())
                            .unwrap_or(false),
                    },
                    _ => bail!("texture {k:?} is neither a string nor an object"),
                };
                textures.push((k.clone(), entry));
            }
        }
        let elements = match o.get("elements") {
            None => None,
            Some(Value::Array(list)) => Some(
                list.iter()
                    .enumerate()
                    .map(|(i, e)| parse_element(e).with_context(|| format!("elements[{i}]")))
                    .collect::<Result<Vec<_>>>()?,
            ),
            Some(_) => bail!("`elements` is not a list"),
        };
        Ok(RawModel {
            parent,
            textures,
            elements,
            ambient_occlusion: o.get("ambientocclusion").and_then(|b| b.as_bool()),
            gui_light: o
                .get("gui_light")
                .and_then(|s| s.as_str())
                .map(String::from),
            display: o.get("display").cloned(),
        })
    }
}

fn parse_element(v: &Value) -> Result<Element> {
    let Some(o) = v.as_object() else {
        bail!("element is not an object");
    };
    let from = position(o.get("from").ok_or_else(|| anyhow!("no `from`"))?, "from")?;
    let to = position(o.get("to").ok_or_else(|| anyhow!("no `to`"))?, "to")?;
    let rotation = match o.get("rotation") {
        None | Some(Value::Null) => None,
        Some(r) => Some(parse_rotation(r).context("rotation")?),
    };
    let Some(fo) = o.get("faces").and_then(|f| f.as_object()) else {
        bail!("no `faces`");
    };
    if fo.is_empty() {
        bail!("`faces` is empty: the game wants between 1 and 6 faces");
    }
    let mut faces = Vec::new();
    for (k, f) in fo {
        let dir = Dir::parse(k).ok_or_else(|| anyhow!("unknown face direction {k:?}"))?;
        let fobj = f
            .as_object()
            .ok_or_else(|| anyhow!("face {k} is not an object"))?;
        let uv = match fobj.get("uv") {
            None => None,
            Some(u) => Some(num_array::<4>(u, "uv").with_context(|| format!("face {k}"))?),
        };
        let texture = fobj
            .get("texture")
            .and_then(|t| t.as_str())
            .ok_or_else(|| anyhow!("face {k}: no `texture`"))?
            .to_string();
        // The game reads a name it does not know as "no cull face" (1.13 to 1.21 have a scaffolding
        // model that says "bottom"), so this does too
        let cullface = fobj
            .get("cullface")
            .and_then(|c| c.as_str())
            .and_then(Dir::parse);
        let rot = match fobj.get("rotation") {
            None | Some(Value::Null) => 0,
            Some(r) => whole(r, "rotation").with_context(|| format!("face {k}"))?,
        };
        if rot.rem_euclid(90) != 0 {
            bail!("face {k}: rotation {rot} is not a multiple of 90");
        }
        // A negative tint index means "none" in the game
        let tintindex = match fobj.get("tintindex") {
            None | Some(Value::Null) => None,
            Some(t) => {
                let i = whole(t, "tintindex").with_context(|| format!("face {k}"))?;
                if i > u32::MAX as i64 {
                    bail!("face {k}: tintindex {i} is out of range");
                }
                // a negative tint index means "none" in the game
                (i >= 0).then_some(i as u32)
            }
        };
        faces.push((
            dir,
            Face {
                uv,
                texture,
                cullface,
                rotation: rot.rem_euclid(360) as i32,
                tintindex,
            },
        ));
    }
    let light_emission = match o.get("light_emission") {
        None | Some(Value::Null) => 0,
        Some(n) => {
            let l = whole(n, "light_emission")?;
            if !(0..=15).contains(&l) {
                bail!("`light_emission` is {l}: the game wants a whole number from 0 to 15");
            }
            l as u8
        }
    };
    Ok(Element {
        from,
        to,
        rotation,
        shade: opt_bool(o, "shade")?.unwrap_or(true),
        light_emission,
        faces,
    })
}

/// `origin` is required: the game fails a rotation without one (and every rotation in the eight
/// jars has one). The three Euler angles are each 0 when missing, as in the game.
fn parse_rotation(v: &Value) -> Result<ElementRotation> {
    let Some(o) = v.as_object() else {
        bail!("not an object");
    };
    let origin = num_array::<3>(
        o.get("origin")
            .ok_or_else(|| anyhow!("no `origin` (the game fails a rotation without one)"))?,
        "origin",
    )?;
    let angle_of = |v: &Value, what: &str| -> Result<f64> {
        let a = v
            .as_f64()
            .ok_or_else(|| anyhow!("{what} is not a number"))?;
        if !a.is_finite() {
            bail!("{what} is not finite");
        }
        Ok(a)
    };
    let rescale = opt_bool(o, "rescale")?.unwrap_or(false);
    if o.contains_key("axis") || o.contains_key("angle") {
        let axis = match o.get("axis").and_then(|a| a.as_str()) {
            Some(a) if a.eq_ignore_ascii_case("x") => Axis::X,
            Some(a) if a.eq_ignore_ascii_case("y") => Axis::Y,
            Some(a) if a.eq_ignore_ascii_case("z") => Axis::Z,
            _ => bail!("axis is not x, y or z"),
        };
        let angle = angle_of(
            o.get("angle").ok_or_else(|| anyhow!("no `angle`"))?,
            "angle",
        )?;
        return Ok(ElementRotation::Axis {
            origin,
            axis,
            angle,
            rescale,
        });
    }
    if o.contains_key("x") || o.contains_key("y") || o.contains_key("z") {
        let g = |k: &str| -> Result<f64> {
            match o.get(k) {
                None => Ok(0.0),
                Some(v) => angle_of(v, k),
            }
        };
        return Ok(ElementRotation::Euler {
            origin,
            x: g("x")?,
            y: g("y")?,
            z: g("z")?,
            rescale,
        });
    }
    bail!("neither axis and angle, nor x, y, z")
}

/// A parent that is not a file: the game's built-in model roots
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Builtin {
    /// `builtin/generated`: the game builds a flat mesh from the `layerN` textures
    Generated,
    /// `builtin/entity`: drawn by code (chests, banners, shields, skulls ...)
    Entity,
    /// Any other `builtin/...` root
    Other(String),
}

#[derive(Clone, Debug)]
pub struct ResolvedModel {
    /// The model asked for, as `block/stone` (no namespace, no extension)
    pub id: String,
    /// The ids visited from this model up to the root
    pub chain: Vec<String>,
    pub builtin: Option<Builtin>,
    /// Merged: the root's first, children's overriding by name
    pub textures: Vec<(String, TexEntry)>,
    pub elements: Vec<Element>,
    pub ambient_occlusion: bool,
    pub gui_light: Option<String>,
    pub display: Option<Value>,
}

/// `minecraft:block/stone` and `block/stone` are the same; other namespaces are not in the jar
pub fn normalize_location(s: &str) -> Result<String> {
    match s.split_once(':') {
        None => Ok(s.to_string()),
        Some(("minecraft", rest)) => Ok(rest.to_string()),
        Some((ns, _)) => bail!("namespace {ns:?}: only `minecraft` assets are in the client jar"),
    }
}

const MAX_DEPTH: usize = 32;

/// How many `layerN` textures a generated item reads: `layer0` to `layer4`
pub const MAX_LAYERS: u32 = 5;

/// Walks `parent` links from `id` (`block/stone`, a path relative to `models/`) and merges.
/// `load` returns the parsed file for a path relative to `models/`.
/// Rules that differ between versions of the game
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResolveRules {
    /// A model whose `elements` is an empty list takes its parent's elements (1.8 to 1.12 do:
    /// they treat "none" and "empty" alike). In 26.x an empty list is a real, empty geometry that
    /// replaces the parent's.
    pub empty_elements_inherit: bool,
}

pub fn resolve(
    id: &str,
    rules: ResolveRules,
    load: &mut dyn FnMut(&str) -> Result<Arc<RawModel>>,
) -> Result<ResolvedModel> {
    let id = normalize_location(id)?;
    let mut files: Vec<Arc<RawModel>> = Vec::new();
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut builtin = None;
    let mut cur = id.clone();
    loop {
        if !seen.insert(cur.clone()) {
            bail!("{id}: parent loop at {cur}");
        }
        if files.len() >= MAX_DEPTH {
            bail!("{id}: more than {MAX_DEPTH} parents");
        }
        if let Some(root) = cur.strip_prefix("builtin/") {
            builtin = Some(match root {
                "generated" => Builtin::Generated,
                "entity" => Builtin::Entity,
                other => Builtin::Other(other.to_string()),
            });
            chain.push(cur);
            break;
        }
        let m = load(&cur).with_context(|| format!("model {cur} (from {id})"))?;
        chain.push(cur.clone());
        let parent = m.parent.clone();
        files.push(m);
        match parent {
            Some(p) => cur = normalize_location(&p)?,
            None => break,
        }
    }
    let mut textures: Vec<(String, TexEntry)> = Vec::new();
    let mut elements = Vec::new();
    let mut ambient = true;
    let mut gui_light = None;
    let mut display = None;
    for m in files.iter().rev() {
        for (k, t) in &m.textures {
            match textures.iter_mut().find(|(n, _)| n == k) {
                Some(slot) => slot.1 = t.clone(),
                None => textures.push((k.clone(), t.clone())),
            }
        }
        if let Some(e) = &m.elements {
            if !(e.is_empty() && rules.empty_elements_inherit) {
                elements = e.clone();
            }
        }
        if let Some(a) = m.ambient_occlusion {
            ambient = a;
        }
        if m.gui_light.is_some() {
            gui_light = m.gui_light.clone();
        }
        if m.display.is_some() {
            display = m.display.clone();
        }
    }
    Ok(ResolvedModel {
        id,
        chain,
        builtin,
        textures,
        elements,
        ambient_occlusion: ambient,
        gui_light,
        display,
    })
}

/// A texture a face points at, after following `#name` links
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TexturePath {
    /// Relative to `textures/`, no namespace, no extension: `block/stone`
    pub path: String,
    pub translucent: bool,
}

impl ResolvedModel {
    /// What a face's `texture` names. In the game it is the name of a texture **slot**, written
    /// with or without the leading `#` (`"all"` and `"#all"` are the same), never a path; the slot's
    /// value is a path, or another slot written `#other`, which is followed. A name no slot defines
    /// is an error: the game draws its built-in missing-texture pattern there.
    pub fn texture(&self, reference: &str) -> Result<TexturePath> {
        if reference.is_empty() {
            bail!("{}: empty texture reference", self.id);
        }
        let mut name = reference.strip_prefix('#').unwrap_or(reference).to_string();
        let mut translucent = false;
        for _ in 0..MAX_DEPTH {
            let Some((_, e)) = self.textures.iter().find(|(n, _)| *n == name) else {
                bail!("{}: texture slot #{name} is not defined", self.id);
            };
            translucent |= e.force_translucent;
            match e.value.strip_prefix('#') {
                Some(next) => name = next.to_string(),
                None => {
                    return Ok(TexturePath {
                        path: normalize_location(&e.value)?,
                        translucent,
                    })
                }
            }
        }
        bail!("{}: texture reference {reference} loops", self.id)
    }

    /// The `layerN` slots a generated item draws: `layer0` to `layer4`, in order, stopping at the
    /// first one that is missing (the game does not look past a gap, or past `layer4`)
    pub fn layers(&self) -> Vec<(u32, &str)> {
        let mut out = Vec::new();
        for i in 0..MAX_LAYERS {
            let name = format!("layer{i}");
            match self.textures.iter().find(|(n, _)| *n == name) {
                Some((_, e)) => out.push((i, e.value.as_str())),
                None => break,
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn files(list: &[(&str, &str)]) -> HashMap<String, String> {
        list.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    fn res(id: &str, f: &HashMap<String, String>) -> Result<ResolvedModel> {
        resolve(id, ResolveRules::default(), &mut |p| {
            let t = f.get(p).ok_or_else(|| anyhow!("{p}: no such model"))?;
            Ok(Arc::new(RawModel::parse(t)?))
        })
    }

    const CUBE: &str = r##"{"textures":{"particle":"#all"},"elements":[{"from":[0,0,0],"to":[16,16,16],
        "faces":{"down":{"texture":"#all","cullface":"down"},"up":{"texture":"#all","tintindex":0,"uv":[0,0,8,8]}}}]}"##;

    #[test]
    fn parent_chain_merges_textures_and_inherits_elements() {
        let f = files(&[
            ("block/cube", CUBE),
            (
                "block/cube_all",
                r#"{"parent":"block/cube","textures":{"extra":"x/y"}}"#,
            ),
            (
                "block/stone",
                r#"{"parent":"minecraft:block/cube_all","textures":{"all":"minecraft:block/stone"}}"#,
            ),
        ]);
        let m = res("block/stone", &f).unwrap();
        assert_eq!(m.chain, ["block/stone", "block/cube_all", "block/cube"]);
        assert_eq!(m.elements.len(), 1);
        assert_eq!(m.elements[0].faces.len(), 2);
        assert_eq!(m.texture("#all").unwrap().path, "block/stone");
        assert_eq!(m.texture("#particle").unwrap().path, "block/stone");
        assert_eq!(m.texture("#extra").unwrap().path, "x/y");
        assert!(m.texture("#nope").is_err());
        let up = &m.elements[0].faces[1].1;
        assert_eq!(up.tintindex, Some(0));
        assert_eq!(up.uv, Some([0.0, 0.0, 8.0, 8.0]));
        assert_eq!(m.elements[0].faces[0].1.cullface, Some(Dir::Down));
    }

    #[test]
    fn child_elements_replace_parent_elements() {
        let f = files(&[
            ("block/cube", CUBE),
            (
                "block/slab",
                r##"{"parent":"block/cube","elements":[{"from":[0,0,0],"to":[16,8,16],"faces":{"up":{"texture":"#all"}}}]}"##,
            ),
        ]);
        let m = res("block/slab", &f).unwrap();
        assert_eq!(m.elements.len(), 1);
        assert_eq!(m.elements[0].to, [16.0, 8.0, 16.0]);
    }

    #[test]
    fn child_texture_overrides_by_name() {
        let f = files(&[
            ("a", r#"{"textures":{"all":"one"}}"#),
            ("b", r#"{"parent":"a","textures":{"all":"two"}}"#),
        ]);
        assert_eq!(res("b", &f).unwrap().texture("#all").unwrap().path, "two");
    }

    #[test]
    fn variable_chains_and_loops() {
        let f = files(&[(
            "a",
            r##"{"textures":{"a":"#b","b":"#c","c":"t/x","l1":"#l2","l2":"#l1"}}"##,
        )]);
        let m = res("a", &f).unwrap();
        assert_eq!(m.texture("#a").unwrap().path, "t/x");
        assert!(m.texture("#l1").is_err());
        // a face names a slot, with or without the `#`; a path is not a slot
        assert_eq!(m.texture("a").unwrap().path, "t/x");
        assert!(m.texture("block/direct").is_err());
    }

    #[test]
    fn a_face_names_a_slot_with_or_without_the_hash() {
        // Mojang's heavy_core model writes `"texture": "all"` and means the slot `all`
        let f = files(&[(
            "m",
            r##"{"textures":{"all":"block/heavy_core","particle":"#all"},"elements":[{"from":[0,0,0],"to":[16,16,16],
                "faces":{"north":{"texture":"all"},"up":{"texture":"#all"},"down":{"texture":"particle"}}}]}"##,
        )]);
        let m = res("m", &f).unwrap();
        for (_, face) in &m.elements[0].faces {
            assert_eq!(m.texture(&face.texture).unwrap().path, "block/heavy_core");
        }
        assert!(m.texture("nothing").is_err());
        assert!(m.texture("#nothing").is_err());
        assert!(m.texture("").is_err());
    }

    #[test]
    fn layers_run_from_layer0_to_layer4_and_stop_at_the_first_gap() {
        let f = files(&[(
            "m",
            r##"{"textures":{"layer0":"a","layer1":"b","layer3":"d","layer2":"c","layer4":"e","layer5":"f"}}"##,
        )]);
        let names: Vec<_> = res("m", &f)
            .unwrap()
            .layers()
            .iter()
            .map(|l| l.1.to_string())
            .collect();
        assert_eq!(names, ["a", "b", "c", "d", "e"], "layer5 is not read");
        let f = files(&[("g", r##"{"textures":{"layer0":"a","layer2":"c"}}"##)]);
        assert_eq!(
            res("g", &f).unwrap().layers().len(),
            1,
            "a gap ends the list"
        );
        let f = files(&[("h", r##"{"textures":{"layer1":"b"}}"##)]);
        assert!(res("h", &f).unwrap().layers().is_empty());
    }

    #[test]
    fn an_empty_elements_list_follows_the_versions_rule() {
        let f = files(&[("p", CUBE), ("c", r##"{"parent":"p","elements":[]}"##)]);
        let get = |inherit| {
            resolve(
                "c",
                ResolveRules {
                    empty_elements_inherit: inherit,
                },
                &mut |p| {
                    let t = f.get(p).ok_or_else(|| anyhow!("{p}: no such model"))?;
                    Ok(Arc::new(RawModel::parse(t)?))
                },
            )
            .unwrap()
        };
        // 1.8 to 1.12: an empty list is the same as none, the parent's elements stay
        assert_eq!(get(true).elements.len(), 1);
        // 26.x: an empty list is a geometry of its own
        assert_eq!(get(false).elements.len(), 0);
    }

    #[test]
    fn faces_and_light_follow_the_games_checks() {
        assert!(
            RawModel::parse(r##"{"elements":[{"from":[0,0,0],"to":[1,1,1],"faces":{}}]}"##)
                .is_err()
        );
        assert!(RawModel::parse(elem(r#","light_emission":16"#)).is_err());
        assert!(RawModel::parse(elem(r#","light_emission":15"#)).is_ok());
        // a face rotation of 360 or -90 is read modulo 360, as the game does
        let m = RawModel::parse(
            r##"{"elements":[{"from":[0,0,0],"to":[1,1,1],"faces":{"up":{"texture":"#a","rotation":360},"down":{"texture":"#a","rotation":-90}}}]}"##,
        )
        .unwrap();
        let rots: Vec<i32> = m.elements.unwrap()[0]
            .faces
            .iter()
            .map(|f| f.1.rotation)
            .collect();
        assert_eq!(rots, [0, 270]);
    }

    #[test]
    fn builtin_roots() {
        let f = files(&[
            ("item/generated", r#"{"parent":"builtin/generated"}"#),
            (
                "item/sword",
                r#"{"parent":"item/generated","textures":{"layer0":"item/sword"}}"#,
            ),
            ("item/chest", r#"{"parent":"builtin/entity"}"#),
        ]);
        let m = res("item/sword", &f).unwrap();
        assert_eq!(m.builtin, Some(Builtin::Generated));
        assert_eq!(m.layers(), vec![(0, "item/sword")]);
        assert_eq!(
            res("item/chest", &f).unwrap().builtin,
            Some(Builtin::Entity)
        );
    }

    #[test]
    fn parent_loop_and_missing_parent() {
        let f = files(&[
            ("a", r#"{"parent":"b"}"#),
            ("b", r#"{"parent":"a"}"#),
            ("c", r#"{"parent":"nope"}"#),
        ]);
        assert!(res("a", &f).unwrap_err().to_string().contains("loop"));
        assert!(res("c", &f).is_err());
    }

    #[test]
    fn force_translucent_is_carried_through_the_reference() {
        let f = files(&[(
            "a",
            r##"{"textures":{"line":{"sprite":"block/dust","force_translucent":true},"x":"#line"}}"##,
        )]);
        let m = res("a", &f).unwrap();
        assert_eq!(
            m.texture("#x").unwrap(),
            TexturePath {
                path: "block/dust".into(),
                translucent: true
            }
        );
    }

    #[test]
    fn rotations_parse_both_forms() {
        let m = RawModel::parse(
            r##"{"elements":[
              {"from":[0,0,0],"to":[1,1,1],"rotation":{"origin":[8,8,8],"axis":"y","angle":-22.5,"rescale":true},"faces":{"up":{"texture":"#a"}}},
              {"from":[0,0,0],"to":[1,1,1],"rotation":{"x":180,"y":-67.5,"z":-180,"origin":[8,0,8]},"faces":{"up":{"texture":"#a"}}}]}"##,
        )
        .unwrap();
        let e = m.elements.unwrap();
        assert!(
            matches!(e[0].rotation, Some(ElementRotation::Axis { angle, rescale: true, .. }) if angle == -22.5)
        );
        assert!(
            matches!(e[1].rotation, Some(ElementRotation::Euler { x, y, z, .. }) if x == 180.0 && y == -67.5 && z == -180.0)
        );
    }

    #[test]
    fn an_unknown_cullface_name_means_no_cull_face() {
        let m = RawModel::parse(
            r##"{"elements":[{"from":[0,0,0],"to":[1,1,1],"faces":{"down":{"texture":"#a","cullface":"bottom"}}}]}"##,
        )
        .unwrap();
        assert_eq!(m.elements.unwrap()[0].faces[0].1.cullface, None);
    }

    #[test]
    fn bad_models_are_errors() {
        assert!(
            RawModel::parse(r##"{"elements":[{"from":[0,0],"to":[1,1,1],"faces":{}}]}"##).is_err()
        );
        assert!(RawModel::parse(r##"{"elements":[{"from":[0,0,0],"to":[1,1,1],"faces":{"sideways":{"texture":"#a"}}}]}"##).is_err());
        assert!(RawModel::parse("[]").is_err());
        assert!(normalize_location("other:thing").is_err());
    }

    fn elem(extra: &str) -> String {
        format!(
            r##"{{"elements":[{{"from":[0,0,0],"to":[16,16,16]{extra},"faces":{{"up":{{"texture":"#a"}}}}}}]}}"##
        )
    }

    #[test]
    fn from_and_to_must_be_finite_and_in_the_games_range() {
        assert!(RawModel::parse(elem("")).is_ok());
        for bad in [
            r##"{"elements":[{"from":[0,0,0],"to":[1e300,1,1],"faces":{"up":{"texture":"#a"}}}]}"##,
            r##"{"elements":[{"from":[-16.5,0,0],"to":[1,1,1],"faces":{"up":{"texture":"#a"}}}]}"##,
            r##"{"elements":[{"from":[0,0,0],"to":[1,32.5,1],"faces":{"up":{"texture":"#a"}}}]}"##,
        ] {
            let e = RawModel::parse(bad).unwrap_err();
            assert!(
                format!("{e:#}").contains("range") || format!("{e:#}").contains("accept"),
                "{e:#}"
            );
        }
        // the edges of the range are fine
        assert!(RawModel::parse(r##"{"elements":[{"from":[-16,-16,-16],"to":[32,32,32],"faces":{"up":{"texture":"#a"}}}]}"##).is_ok());
        // 1e999 is not a number serde_json will hand over
        assert!(RawModel::parse(
            r##"{"elements":[{"from":[0,0,0],"to":[1e999,1,1],"faces":{"up":{"texture":"#a"}}}]}"##
        )
        .is_err());
        // uv too
        assert!(RawModel::parse(r##"{"elements":[{"from":[0,0,0],"to":[1,1,1],"faces":{"up":{"texture":"#a","uv":[0,0,1e999,1]}}}]}"##).is_err());
    }

    #[test]
    fn a_rotation_without_an_origin_is_an_error_not_the_block_centre() {
        let e = RawModel::parse(elem(r#","rotation":{"axis":"y","angle":22.5}"#)).unwrap_err();
        assert!(format!("{e:#}").contains("origin"), "{e:#}");
        // a missing Euler angle is 0, as in the game
        let m = RawModel::parse(elem(r#","rotation":{"origin":[8,8,8],"x":10,"y":20}"#)).unwrap();
        assert!(matches!(
            m.elements.unwrap()[0].rotation,
            Some(ElementRotation::Euler { x, y, z, .. }) if x == 10.0 && y == 20.0 && z == 0.0
        ));
        assert!(RawModel::parse(elem(
            r#","rotation":{"origin":[8,8,8],"axis":"y","angle":22.5,"rescale":true}"#
        ))
        .is_ok());
        // rescale is defined past 45 degrees too (1 / the largest component of the turned axis)
        assert!(RawModel::parse(elem(
            r#","rotation":{"origin":[8,8,8],"axis":"y","angle":60,"rescale":true}"#
        ))
        .is_ok());
    }

    #[test]
    fn whole_numbers_written_with_a_fraction_part_are_read_as_the_game_does() {
        let m = RawModel::parse(
            r##"{"elements":[{"from":[0,0,0],"to":[1,1,1],"faces":{"up":{"texture":"#a","rotation":90.0,"tintindex":1.0}}}]}"##,
        )
        .unwrap();
        let f = &m.elements.unwrap()[0].faces[0].1;
        assert_eq!(f.rotation, 90);
        assert_eq!(f.tintindex, Some(1));
        for bad in [
            r##"{"elements":[{"from":[0,0,0],"to":[1,1,1],"faces":{"up":{"texture":"#a","rotation":45.5}}}]}"##,
            r##"{"elements":[{"from":[0,0,0],"to":[1,1,1],"faces":{"up":{"texture":"#a","rotation":"90"}}}]}"##,
            r##"{"elements":[{"from":[0,0,0],"to":[1,1,1],"faces":{"up":{"texture":"#a","tintindex":0.5}}}]}"##,
            r##"{"elements":[{"from":[0,0,0],"to":[1,1,1],"shade":"no","faces":{"up":{"texture":"#a"}}}]}"##,
        ] {
            assert!(RawModel::parse(bad).is_err(), "{bad}");
        }
    }
}
