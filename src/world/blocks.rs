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
//! # Tints: only where the data says
//!
//! Which blocks use which biome colormap is in the game's code. This crate applies a biome tint to a
//! block **only where the jar's data says so**, and there is one such place: a 26.x item definition
//! (`items/<name>.json`) whose `tints` hold a `minecraft:grass` entry says that the item, and so the
//! block of the same name, is coloured from the grass colormap ([`data_tint`]). Nothing else in the
//! jars says it (searched in 26.3 and 1.21.1: the item definitions, the models and blockstates, tags,
//! worldgen and the biome files were read for `foliage`, `tint` and `colormap`; leaves and vines have
//! a `minecraft:constant` item tint, the water cauldron has no item of its own, and the biome files
//! only hold the colours, not which block takes them). So:
//!
//! - grass blocks, ferns, tall grass and the like are tinted in jars that have item definitions
//!   (1.21.4 and later, 26.x);
//! - everything else with a `tintindex` (foliage on leaves and vines, the water cauldron, redstone
//!   dust, stems, the lily pad, spruce and birch leaves) is **left untinted** and counted in the
//!   report as "biome tint left out: the block-to-tint mapping is in the game's code";
//! - in a jar with no item definitions (1.13 to 1.21.3) nothing is tinted at all, grass included.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use serde_json::Value;

use super::chunk::PaletteEntry;
use crate::assets::{Assets, LeftOut};
use crate::blockstate::State;
use crate::model::Dir;
use crate::texture::{AlphaClass, Rgba};
use crate::tint::ColormapKind;
use crate::version::Layout;

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

/// The biome colormap the data says the faces with a `tintindex` of this block use: the grass one,
/// where the item of the same name has a `minecraft:grass` tint in its item definition (1.21.4 and
/// later). `None` is "the data does not say".
pub fn data_tint(assets: &Assets, local_name: &str) -> Option<ColormapKind> {
    if assets.layout() != Layout::ModelsWithItemDefs {
        return None;
    }
    fn has_grass(v: &Value) -> bool {
        match v {
            Value::Object(m) => {
                m.get("type").and_then(Value::as_str) == Some("minecraft:grass")
                    || m.values().any(has_grass)
            }
            Value::Array(l) => l.iter().any(has_grass),
            _ => false,
        }
    }
    let def = assets.item_definition(local_name).ok()?;
    if has_grass(&def) {
        Some(ColormapKind::Grass)
    } else {
        None
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
    pub tint: Option<ColormapKind>,
    pub fate: Fate,
    /// The state says `waterlogged=true`: the water is a fluid, which is left out
    pub waterlogged: bool,
    /// What the bake said about this state: the entry used when none was given, the variants a
    /// partial state fits, faces left out, properties the blockstate does not mention. The report
    /// counts them.
    pub notes: Vec<String>,
}

pub struct StateTable<'a> {
    assets: &'a Assets,
    pub states: Vec<StateEntry>,
    index: HashMap<(String, String), u16>,
    pub textures: Vec<TexInfo>,
    tex_index: HashMap<String, u32>,
    known: HashSet<String>,
    /// Quads held by all the states so far, and the most they may hold (a state is a copy of its
    /// model's quads, and a hostile palette could ask for many)
    state_quads: usize,
    max_state_quads: usize,
    /// Per block name: why a bare palette name cannot be drawn (`None`: it can)
    bare: HashMap<String, Option<String>>,
}

/// Quads (about 110 bytes each) all the block states of one load may hold. The jars here need under
/// 100,000 for a whole region.
pub const MAX_STATE_QUADS: usize = 2_000_000;

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
            state_quads: 0,
            max_state_quads: MAX_STATE_QUADS,
            bare: HashMap::new(),
        }
    }

    /// A different budget of quads for all the states (the default is [`MAX_STATE_QUADS`])
    pub fn with_quad_budget(mut self, n: usize) -> Self {
        self.max_state_quads = n;
        self
    }

    pub fn assets(&self) -> &Assets {
        self.assets
    }

    /// The id of a palette entry of a save (see [`Self::resolve`], and `bare` below)
    pub fn resolve_entry(&mut self, e: &PaletteEntry) -> Result<u16> {
        self.resolve_inner(&e.name, &e.props, e.bare)
    }

    /// The id of a block state given with all its properties, baking it the first time it is met
    pub fn resolve(&mut self, name: &str, props: &[(String, String)]) -> Result<u16> {
        self.resolve_inner(name, props, false)
    }

    /// Why a bare palette name of this block cannot be drawn: `None` when it can, because every
    /// state its blockstate has bakes to the same quads (or it has one state), so which one the
    /// game's default is does not matter
    fn bare_ambiguity(&mut self, local: &str) -> Option<String> {
        if let Some(r) = self.bare.get(local) {
            return r.clone();
        }
        let r = self.compute_bare_ambiguity(local);
        self.bare.insert(local.to_string(), r.clone());
        r
    }

    fn compute_bare_ambiguity(&self, local: &str) -> Option<String> {
        const MAX_KEYS: usize = 1024;
        let Ok(bs) = self.assets.blockstate(local) else {
            return Some("its blockstate cannot be read".into());
        };
        if bs.is_multipart() {
            let conditional = bs.multipart.iter().filter(|p| p.when.is_some()).count();
            return (conditional > 0).then(|| {
                format!("its blockstate has {conditional} part(s) that depend on the state")
            });
        }
        if bs.variants.len() <= 1 {
            return None;
        }
        if bs.variants.len() > MAX_KEYS {
            return Some(format!("its blockstate has {} states", bs.variants.len()));
        }
        let mut results: Vec<Vec<crate::bake::Quad>> = Vec::new();
        for (key, _) in &bs.variants {
            let quads = State::parse(key)
                .and_then(|st| self.assets.bake_block(local, &st))
                .map(|b| b.quads().cloned().collect::<Vec<_>>());
            match quads {
                Ok(q) => {
                    if !results.contains(&q) {
                        results.push(q);
                    }
                }
                Err(e) => return Some(format!("its state `{key}` does not bake: {e:#}")),
            }
        }
        (results.len() > 1).then(|| {
            format!(
                "its blockstate has {} states that bake to {} different results",
                bs.variants.len(),
                results.len()
            )
        })
    }

    /// The id of a block state, baking it the first time it is met.
    ///
    /// States are keyed by what the blockstate file can tell apart: the properties it mentions, and
    /// `waterlogged` (which the report counts). A palette that adds properties the file does not
    /// mention, or any number of them, is the same state, so it cannot fill the table.
    ///
    /// `bare` is a 26.x palette entry that is only a name: the block's default state, which is in
    /// the game's code. Such an entry is drawn only when no state of the block would look different;
    /// otherwise it is left out, with the reason.
    fn resolve_inner(&mut self, name: &str, props: &[(String, String)], bare: bool) -> Result<u16> {
        let local = name.strip_prefix("minecraft:");
        if let Some(l) = local {
            if air_like(l) {
                return Ok(AIR);
            }
        }
        if bare {
            if let Some(l) = local.filter(|l| self.known.contains(*l)) {
                if let Some(why) = self.bare_ambiguity(l) {
                    let key = (name.to_string(), ";bare".to_string());
                    if let Some(&id) = self.index.get(&key) {
                        return Ok(id);
                    }
                    if self.states.len() >= MAX_STATES {
                        anyhow::bail!(
                            "the world uses more than {MAX_STATES} distinct block states"
                        );
                    }
                    let id = self.states.len() as u16;
                    self.states.push(StateEntry {
                        name: Arc::from(name),
                        props: "(bare name)".into(),
                        quads: Vec::new(),
                        covers: [false; 6],
                        tint: None,
                        fate: Fate::LeftOut(format!(
                            "default state is in the game's code: the palette gives only the name, and {why}, so which state the game means is not in the data"
                        )),
                        waterlogged: false,
                        notes: Vec::new(),
                    });
                    self.index.insert(key, id);
                    return Ok(id);
                }
            }
        }
        let waterlogged = props.iter().any(|(k, v)| k == "waterlogged" && v == "true");
        let known_name = local.filter(|l| self.known.contains(*l));
        let (kept, ignored): (Vec<(String, String)>, Vec<String>) =
            match known_name.and_then(|l| self.assets.blockstate(l).ok()) {
                Some(bs) => {
                    let (k, ig) = bs.split_known(&State(props.iter().cloned().collect()));
                    (k.0.into_iter().collect(), ig)
                }
                // not in the jar, or unreadable: one state for the name
                None => (Vec::new(), Vec::new()),
            };
        let mut key_props = kept
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(",");
        if waterlogged && !kept.iter().any(|(k, _)| k == "waterlogged") {
            key_props.push_str(";waterlogged=true");
        }
        let key = (name.to_string(), key_props);
        if let Some(&id) = self.index.get(&key) {
            return Ok(id);
        }
        if self.states.len() >= MAX_STATES {
            anyhow::bail!("the world uses more than {MAX_STATES} distinct block states");
        }
        let entry = self.prepare(
            name,
            known_name.is_some(),
            local.is_some(),
            &kept,
            ignored,
            waterlogged,
        );
        if self.state_quads + entry.quads.len() > self.max_state_quads {
            anyhow::bail!(
                "the block states of the world need more than {} quads",
                self.max_state_quads
            );
        }
        self.state_quads += entry.quads.len();
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
        in_jar: bool,
        minecraft: bool,
        kept: &[(String, String)],
        ignored: Vec<String>,
        waterlogged: bool,
    ) -> StateEntry {
        let mut e = StateEntry {
            name: Arc::from(name),
            props: kept
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(","),
            quads: Vec::new(),
            covers: [false; 6],
            tint: None,
            fate: Fate::Drawn,
            waterlogged,
            notes: Vec::new(),
        };
        let local = name.strip_prefix("minecraft:").unwrap_or(name);
        if !minecraft {
            e.fate = Fate::Unknown(format!(
                "namespace of {name}: only `minecraft` blocks are in the client jar"
            ));
            return e;
        }
        if !in_jar {
            e.fate = Fate::Unknown(format!(
                "no blockstate `{local}` in the jar of Minecraft {}",
                self.assets.version().id
            ));
            return e;
        }
        let state = State(kept.iter().cloned().collect());
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
        if !ignored.is_empty() {
            e.notes.push(format!(
                "the blockstate does not mention {}: ignored",
                ignored.join(", ")
            ));
        }
        let rule = data_tint(self.assets, local);
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
    fn only_the_item_definitions_say_which_block_is_tinted() {
        // the test jar has an item definition with a grass tint for `grass_block` and a constant one
        // for `oak_leaves`
        let a = crate::world::testworld::world_jar();
        assert_eq!(a.layout(), Layout::ModelsWithItemDefs);
        assert_eq!(data_tint(&a, "grass_block"), Some(ColormapKind::Grass));
        assert_eq!(data_tint(&a, "oak_leaves"), None);
        assert_eq!(data_tint(&a, "stone"), None, "no item definition");
        assert_eq!(data_tint(&a, "no_such_block"), None);
        // a jar without item definitions says nothing, grass included
        assert_eq!(data_tint(&tiny(), "grass_block"), None);
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
    fn properties_the_blockstate_does_not_mention_do_not_make_new_states() {
        let a = crate::world::testworld::world_jar();
        let mut t = StateTable::new(&a);
        let first = t
            .resolve("minecraft:stone", &[("junk0".into(), "x".into())])
            .unwrap();
        for i in 0..3000 {
            let p = [(format!("junk{i}"), "x".to_string())];
            assert_eq!(t.resolve("minecraft:stone", &p).unwrap(), first);
        }
        assert_eq!(t.states.len(), 2, "air and stone");
        assert!(t.states[first as usize]
            .notes
            .iter()
            .any(|n| n.contains("does not mention junk0")));
        // unknown names are one state each, however many properties they carry
        let u = t
            .resolve("minecraft:nothing", &[("a".into(), "b".into())])
            .unwrap();
        assert_eq!(
            u,
            t.resolve("minecraft:nothing", &[("c".into(), "d".into())])
                .unwrap()
        );
        // `waterlogged` is kept apart, the report counts it
        let w = t
            .resolve("minecraft:stone", &[("waterlogged".into(), "true".into())])
            .unwrap();
        assert_ne!(w, first);
        assert!(t.states[w as usize].waterlogged);
        // a property the file does mention does make a state
        let b = t
            .resolve("minecraft:oak_slab", &[("type".into(), "bottom".into())])
            .unwrap();
        let top = t
            .resolve("minecraft:oak_slab", &[("type".into(), "top".into())])
            .unwrap();
        assert_ne!(b, top);
    }

    #[test]
    fn the_states_of_one_load_share_a_budget_of_quads() {
        let a = crate::world::testworld::world_jar();
        let mut t = StateTable::new(&a).with_quad_budget(10);
        t.resolve("minecraft:stone", &[]).unwrap();
        let e = t.resolve("minecraft:glass", &[]).unwrap_err().to_string();
        assert!(e.contains("more than 10 quads"), "{e}");
        let mut t = StateTable::new(&a).with_quad_budget(100);
        let e = t.resolve("minecraft:many", &[]).unwrap_err().to_string();
        assert!(e.contains("more than 100 quads"), "{e}");
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
