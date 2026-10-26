//! Block states of a world, baked once each, with the two facts a world needs that a lone block does
//! not: whether the state **fully covers** a side of its cell (so a neighbour can drop the face that
//! looks at it), and which biome colormap (if any) its tinted faces use.
//!
//! # Occlusion: the rule is ours
//!
//! In the game, whether a block hides the face of its neighbour is a flag in code (`canOcclude`, and
//! the block's collision/occlusion shape). The data holds none of that, so this crate derives it from
//! the one thing the data does hold, the baked geometry:
//!
//! > A face with a `cullface` is dropped when the block on that side has quads that lie exactly in
//! > the plane between the two cells, face the cell the face belongs to, are all of **opaque**
//! > textures (no texel of the texture is transparent, and the data does not call it translucent),
//! > and together **cover the whole unit square**.
//!
//! Where this differs from the game: it never drops a face because the two blocks are the same kind
//! (the game does that for glass, ice and leaves with fancy graphics, a flag in code); a texture with
//! a single transparent texel anywhere never occludes (the game may occlude with a block whose
//! texture has holes only where the block's shape says it does not matter); and a block whose
//! occlusion shape is full but whose model is not (none in the jars here) does not occlude. A face
//! with no `cullface` is always kept. The effect is more faces than the game draws, never fewer than
//! it shows: nothing that is visible in the game is dropped.
//!
//! # Tints: the table is ours
//!
//! Which blocks use which biome colormap is in the game's code, not in the data. [`tint_rule`] is a
//! small table written from what the game does for the blocks whose models carry a `tintindex`, and
//! it is the only place in this crate that says it. 26.x item definitions confirm the `grass` entries
//! (every item with a `minecraft:grass` tint is in the table; a real-data test checks it); the foliage
//! entries are not in any data. Blocks with a `tintindex` that are not in the table (spruce and birch
//! leaves, whose colour is a constant in code, redstone dust by power, stems by age, the lily pad) are
//! left untinted and listed in the report.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;

use crate::assets::{Assets, LeftOut};
use crate::blockstate::State;
use crate::model::Dir;
use crate::texture::{AlphaClass, Rgba};
use crate::tint::ColormapKind;

/// The state id of "no block": air, and anything outside the loaded chunks
pub const AIR: u16 = 0;

/// Most distinct block states one load may meet (ids are 16 bits). A real world has hundreds.
pub const MAX_STATES: usize = 60_000;

/// Directions in the order of [`Dir::ALL`]; the opposite of `i` is `i ^ 1`
pub fn dir_index(d: Dir) -> usize {
    match d {
        Dir::Down => 0,
        Dir::Up => 1,
        Dir::North => 2,
        Dir::South => 3,
        Dir::West => 4,
        Dir::East => 5,
    }
}

/// Where the colour of a block's tinted faces comes from
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TintSource {
    /// A biome colormap, looked up at the biome's temperature and downfall (or the biome's own colour
    /// where its data overrides it)
    Colormap(ColormapKind),
    /// The biome's `water_color`
    Water,
}

/// The colour source of the faces with a `tintindex` of this block, per the table above
pub fn tint_rule(local_name: &str) -> Option<TintSource> {
    use TintSource::*;
    match local_name {
        // `grass` is the name before 1.20.3; `bush` (1.21.5) is confirmed by its item's `grass` tint
        "grass_block" | "grass" | "short_grass" | "tall_grass" | "fern" | "large_fern"
        | "potted_fern" | "sugar_cane" | "bush" => Some(Colormap(ColormapKind::Grass)),
        "oak_leaves" | "jungle_leaves" | "acacia_leaves" | "dark_oak_leaves" | "vine" => {
            Some(Colormap(ColormapKind::Foliage))
        }
        // the water in a cauldron is the one water the models draw
        "water_cauldron" => Some(Water),
        _ => None,
    }
}

/// A texture the states of this load use, decoded once
#[derive(Clone, Debug)]
pub struct TexInfo {
    pub path: Arc<str>,
    /// The first frame the texture's animation shows
    pub image: Arc<Rgba>,
    /// What the texels alone say; `force_translucent` is per quad
    pub alpha: AlphaClass,
}

/// One quad of a state, in block coordinates (0 to 1)
#[derive(Clone, Debug)]
pub struct PreQuad {
    pub pos: [[f32; 3]; 4],
    pub normal: [f32; 3],
    pub uv: [[f32; 2]; 4],
    /// Index in [`StateTable::textures`]
    pub tex: u32,
    pub translucent: bool,
    pub tint_index: Option<u32>,
    pub cullface: Option<Dir>,
}

/// What became of a state
#[derive(Clone, Debug, PartialEq)]
pub enum Fate {
    /// Nothing to draw and nothing to say: air
    Air,
    /// Has geometry
    Drawn,
    /// The data has no geometry for it (or it could not be baked); the text says why
    LeftOut(String),
    /// The jar has no blockstate by that name: a block of another version, or of a mod
    Unknown(String),
}

#[derive(Clone, Debug)]
pub struct StateEntry {
    /// `minecraft:oak_stairs`
    pub name: Arc<str>,
    /// `facing=east,half=bottom`, sorted
    pub props: String,
    pub quads: Vec<PreQuad>,
    /// Per side ([`dir_index`]): the state's quads cover it, opaquely
    pub covers: [bool; 6],
    pub tint: Option<TintSource>,
    pub fate: Fate,
    /// The state says `waterlogged=true`: the water is a fluid, which is left out
    pub waterlogged: bool,
    /// Faces left out while baking (a missing texture slot), from the bake's own notes
    pub notes: Vec<String>,
}

pub struct StateTable<'a> {
    assets: &'a Assets,
    pub states: Vec<StateEntry>,
    index: HashMap<(String, String), u16>,
    pub textures: Vec<TexInfo>,
    tex_index: HashMap<String, u32>,
    known: HashSet<String>,
}

fn air_like(local: &str) -> bool {
    matches!(local, "air" | "cave_air" | "void_air")
}

impl<'a> StateTable<'a> {
    pub fn new(assets: &'a Assets) -> StateTable<'a> {
        let known: HashSet<String> = assets.block_names().into_iter().collect();
        let air = StateEntry {
            name: Arc::from("minecraft:air"),
            props: String::new(),
            quads: Vec::new(),
            covers: [false; 6],
            tint: None,
            fate: Fate::Air,
            waterlogged: false,
            notes: Vec::new(),
        };
        StateTable {
            assets,
            states: vec![air],
            index: HashMap::new(),
            textures: Vec::new(),
            tex_index: HashMap::new(),
            known,
        }
    }

    pub fn assets(&self) -> &Assets {
        self.assets
    }

    /// The id of a block state, baking it the first time it is met
    pub fn resolve(&mut self, name: &str, props: &[(String, String)]) -> Result<u16> {
        let key = (
            name.to_string(),
            props
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(","),
        );
        if let Some(&id) = self.index.get(&key) {
            return Ok(id);
        }
        let local = name.strip_prefix("minecraft:");
        if let Some(l) = local {
            if air_like(l) {
                self.index.insert(key, AIR);
                return Ok(AIR);
            }
        }
        if self.states.len() >= MAX_STATES {
            anyhow::bail!("the world uses more than {MAX_STATES} distinct block states");
        }
        let entry = self.prepare(name, local, &key.1, props);
        let id = self.states.len() as u16;
        self.states.push(entry);
        self.index.insert(key, id);
        Ok(id)
    }

    fn texture(&mut self, path: &Arc<str>) -> Result<u32> {
        if let Some(&i) = self.tex_index.get(&**path) {
            return Ok(i);
        }
        let t = self.assets.texture(path)?;
        let image = t.default_frame();
        let alpha = image.alpha_class(false);
        self.textures.push(TexInfo {
            path: path.clone(),
            image: Arc::new(image),
            alpha,
        });
        let i = (self.textures.len() - 1) as u32;
        self.tex_index.insert(path.to_string(), i);
        Ok(i)
    }

    fn prepare(
        &mut self,
        name: &str,
        local: Option<&str>,
        props_key: &str,
        props: &[(String, String)],
    ) -> StateEntry {
        let mut e = StateEntry {
            name: Arc::from(name),
            props: props_key.to_string(),
            quads: Vec::new(),
            covers: [false; 6],
            tint: None,
            fate: Fate::Drawn,
            waterlogged: props.iter().any(|(k, v)| k == "waterlogged" && v == "true"),
            notes: Vec::new(),
        };
        let Some(local) = local else {
            e.fate = Fate::Unknown(format!(
                "namespace of {name}: only `minecraft` blocks are in the client jar"
            ));
            return e;
        };
        if !self.known.contains(local) {
            e.fate = Fate::Unknown(format!(
                "no blockstate `{local}` in the jar of Minecraft {}",
                self.assets.version().id
            ));
            return e;
        }
        let state = State(props.iter().cloned().collect());
        let baked = match self.assets.bake_block(local, &state) {
            Ok(b) => b,
            Err(err) => {
                e.fate = Fate::LeftOut(format!("could not be baked: {err:#}"));
                return e;
            }
        };
        if baked.is_empty() {
            let why = baked.report.left_out.clone().unwrap_or(LeftOut::NoElements);
            e.fate = Fate::LeftOut(match local {
                "water" | "lava" => {
                    format!("{why}; a fluid: its shape (level, flow) is code, not data")
                }
                _ => why.to_string(),
            });
            return e;
        }
        e.notes = baked.report.notes.clone();
        let rule = tint_rule(local);
        for q in baked.quads() {
            let tex = match self.texture(&q.texture) {
                Ok(t) => t,
                Err(err) => {
                    e.fate =
                        Fate::LeftOut(format!("texture {} could not be read: {err:#}", q.texture));
                    e.quads.clear();
                    return e;
                }
            };
            e.quads.push(PreQuad {
                pos: q.pos,
                normal: q.normal,
                uv: q.uv,
                tex,
                translucent: q.translucent,
                tint_index: q.tint_index,
                cullface: q.cullface,
            });
        }
        if e.quads.iter().any(|q| q.tint_index.is_some()) {
            e.tint = rule;
        }
        e.covers = self.compute_covers(&e.quads);
        e
    }

    fn compute_covers(&self, quads: &[PreQuad]) -> [bool; 6] {
        let mut out = [false; 6];
        for d in Dir::ALL {
            let (axis, plane): (usize, f32) = match d {
                Dir::Down => (1, 0.0),
                Dir::Up => (1, 1.0),
                Dir::North => (2, 0.0),
                Dir::South => (2, 1.0),
                Dir::West => (0, 0.0),
                Dir::East => (0, 1.0),
            };
            let n = d.normal();
            let want = [n[0] as f32, n[1] as f32, n[2] as f32];
            let (a, b) = match axis {
                0 => (1, 2),
                1 => (0, 2),
                _ => (0, 1),
            };
            let mut rects: Vec<[f32; 4]> = Vec::new();
            for q in quads {
                if q.translucent || self.textures[q.tex as usize].alpha != AlphaClass::Opaque {
                    continue;
                }
                if q.normal.iter().zip(want).any(|(x, w)| (x - w).abs() > 1e-4) {
                    continue;
                }
                if q.pos.iter().any(|p| (p[axis] - plane).abs() > 1e-4) {
                    continue;
                }
                let (mut u0, mut u1, mut v0, mut v1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
                for p in &q.pos {
                    u0 = u0.min(p[a]);
                    u1 = u1.max(p[a]);
                    v0 = v0.min(p[b]);
                    v1 = v1.max(p[b]);
                }
                // an axis-aligned rectangle: its area is its bounding box's
                let mut area2 = 0.0f32;
                for i in 0..4 {
                    let (p, q2) = (q.pos[i], q.pos[(i + 1) % 4]);
                    area2 += p[a] * q2[b] - q2[a] * p[b];
                }
                let bbox = (u1 - u0) * (v1 - v0);
                if bbox <= 0.0 || (area2.abs() * 0.5 - bbox).abs() > 1e-4 {
                    continue;
                }
                rects.push([u0, v0, u1, v1]);
            }
            out[dir_index(d)] = covers_unit_square(&rects);
        }
        out
    }
}

/// Whether axis-aligned rectangles `[u0, v0, u1, v1]` together cover the unit square (0 to 1 on both axes)
pub fn covers_unit_square(rects: &[[f32; 4]]) -> bool {
    const EPS: f32 = 1e-4;
    if rects.is_empty() {
        return false;
    }
    let mut xs: Vec<f32> = vec![0.0, 1.0];
    for r in rects {
        for x in [r[0], r[2]] {
            if x > 0.0 && x < 1.0 {
                xs.push(x);
            }
        }
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    xs.dedup_by(|a, b| (*a - *b).abs() < EPS);
    for w in xs.windows(2) {
        if w[1] - w[0] < EPS {
            continue;
        }
        let xm = (w[0] + w[1]) * 0.5;
        let mut spans: Vec<(f32, f32)> = rects
            .iter()
            .filter(|r| r[0] <= xm && xm <= r[2])
            .map(|r| (r[1], r[3]))
            .collect();
        spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut reach = 0.0f32;
        for (s, e) in spans {
            if s > reach + EPS {
                return false;
            }
            reach = reach.max(e);
        }
        if reach < 1.0 - EPS {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::tests::tiny;

    #[test]
    fn coverage_of_the_unit_square() {
        assert!(covers_unit_square(&[[0.0, 0.0, 1.0, 1.0]]));
        assert!(!covers_unit_square(&[]));
        // two halves
        assert!(covers_unit_square(&[
            [0.0, 0.0, 0.5, 1.0],
            [0.5, 0.0, 1.0, 1.0]
        ]));
        // a hole
        assert!(!covers_unit_square(&[
            [0.0, 0.0, 0.5, 1.0],
            [0.6, 0.0, 1.0, 1.0]
        ]));
        // a gap along y
        assert!(!covers_unit_square(&[[0.0, 0.0, 1.0, 0.9]]));
        // four quarters, overlapping a little
        assert!(covers_unit_square(&[
            [0.0, 0.0, 0.55, 0.55],
            [0.5, 0.0, 1.0, 0.55],
            [0.0, 0.5, 0.55, 1.0],
            [0.5, 0.5, 1.0, 1.0],
        ]));
        // a half-height slab side
        assert!(!covers_unit_square(&[[0.0, 0.0, 1.0, 0.5]]));
        // floating point noise from a rotation
        assert!(covers_unit_square(&[[0.0, 0.0, 0.99999, 1.00001]]));
    }

    #[test]
    fn the_tint_table() {
        assert_eq!(
            tint_rule("grass_block"),
            Some(TintSource::Colormap(ColormapKind::Grass))
        );
        assert_eq!(
            tint_rule("oak_leaves"),
            Some(TintSource::Colormap(ColormapKind::Foliage))
        );
        assert_eq!(tint_rule("water_cauldron"), Some(TintSource::Water));
        assert_eq!(tint_rule("birch_leaves"), None);
        assert_eq!(tint_rule("redstone_wire"), None);
    }

    #[test]
    fn states_are_baked_once_and_air_is_zero() {
        let a = tiny();
        let mut t = StateTable::new(&a);
        assert_eq!(t.resolve("minecraft:air", &[]).unwrap(), AIR);
        assert_eq!(t.resolve("minecraft:cave_air", &[]).unwrap(), AIR);
        let c = t.resolve("minecraft:cube", &[]).unwrap();
        assert_eq!(c, t.resolve("minecraft:cube", &[]).unwrap());
        let e = &t.states[c as usize];
        assert_eq!(e.fate, Fate::Drawn);
        // a full cube of an opaque texture covers all six sides
        assert_eq!(e.covers, [true; 6]);
        // unknown names and namespaces
        let u = t.resolve("minecraft:no_such_block", &[]).unwrap();
        assert!(matches!(t.states[u as usize].fate, Fate::Unknown(_)));
        let m = t.resolve("somemod:thing", &[]).unwrap();
        assert!(matches!(&t.states[m as usize].fate, Fate::Unknown(w) if w.contains("namespace")));
        // a block with no geometry in the data is left out with a reason
        let ch = t.resolve("minecraft:chest", &[]).unwrap();
        assert!(matches!(t.states[ch as usize].fate, Fate::LeftOut(_)));
    }

    #[test]
    fn a_cutout_cube_and_a_half_slab_do_not_cover() {
        let a = tiny();
        let mut t = StateTable::new(&a);
        let l = t.resolve("minecraft:leaves", &[]).unwrap();
        assert_eq!(
            t.states[l as usize].covers, [false; 6],
            "a texture with a transparent texel never occludes"
        );
        assert_eq!(t.states[l as usize].tint, None, "no rule for this name");
    }
}
