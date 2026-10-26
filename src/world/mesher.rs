//! Chunks of baked blocks to meshes, with the face culling and the biome tints of [`super::blocks`].
//!
//! The output is plain data ([`WorldMesh`]): per chunk column, vertex arrays grouped by material;
//! materials are shared by the whole world (one image per texture and tint). Positions are in blocks,
//! in the game's axes (+X east, +Y up, +Z south) and the game's block coordinates, so a block at
//! (bx, by, bz) spans bx..bx+1, by..by+1, bz..bz+1 once a chunk's own offset (`cx * 16`, `cz * 16`)
//! is added.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use anyhow::{bail, Result};

use super::biomes::BiomeTable;
use super::blocks::{dir_index, Fate, StateTable, AIR};
use super::grid::{self, BlockGrid, LoadLimits, LoadStats, Sec};
use super::level::LevelInfo;
use super::save::World;
use crate::assets::Assets;
use crate::texture::{AlphaClass, Rgba};

/// The most regions a radius may reach around the centre one
pub const MAX_RADIUS: u32 = 8;

/// What part of the world to turn into a scene
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    /// The region file's coordinates (`r.<x>.<z>.mca`)
    pub region: (i32, i32),
    /// Regions to add on every side (0: just `region`)
    pub radius: u32,
    /// Inclusive chunk coordinates `[x0, z0, x1, z1]` to keep of that, if less than all of it
    pub chunks: Option<[i32; 4]>,
    /// Inclusive block heights `(min, max)` to keep, if less than all of it
    pub y: Option<(i32, i32)>,
}

impl Window {
    pub fn region(x: i32, z: i32) -> Window {
        Window {
            region: (x, z),
            radius: 0,
            chunks: None,
            y: None,
        }
    }

    /// Inclusive chunk coordinates `(x0, z0, x1, z1)`
    pub fn chunk_range(&self) -> Result<(i32, i32, i32, i32)> {
        if self.radius > MAX_RADIUS {
            bail!(
                "a radius of {} regions is over the limit of {MAX_RADIUS}",
                self.radius
            );
        }
        let r = self.radius as i64;
        let c = |region: i32, off: i64| -> Result<i32> {
            i32::try_from((region as i64 + off) * 32 + if off > 0 { 31 } else { 0 }).map_err(|_| {
                anyhow::anyhow!(
                    "region {region} with radius {r} is outside the world's coordinates"
                )
            })
        };
        let (mut x0, mut z0, mut x1, mut z1) = (
            c(self.region.0, -r)?,
            c(self.region.1, -r)?,
            c(self.region.0, r)?,
            c(self.region.1, r)?,
        );
        // a radius of 0 is one region: 32 chunks, not one
        if r == 0 {
            x1 = x0 + 31;
            z1 = z0 + 31;
        }
        if let Some([a, b, cc, d]) = self.chunks {
            if a > cc || b > d {
                bail!("--chunks {a},{b},{cc},{d}: the first corner must not be past the second");
            }
            x0 = x0.max(a);
            z0 = z0.max(b);
            x1 = x1.min(cc);
            z1 = z1.min(d);
            if x0 > x1 || z0 > z1 {
                bail!(
                    "--chunks lies outside the region window (chunks {x0}..{x1} x {z0}..{z1} left)"
                );
            }
        }
        if let Some((lo, hi)) = self.y {
            if lo > hi {
                bail!("--y {lo},{hi}: the minimum must not be above the maximum");
            }
        }
        Ok((x0, z0, x1, z1))
    }
}

#[derive(Clone, Debug)]
pub struct Limits {
    /// Chunks of the window the region tables list
    pub max_chunks: usize,
    /// Bytes the blocks of the window and its border take in memory
    pub max_block_bytes: usize,
    /// Quads in the whole scene (about 100 bytes each as vertex data)
    pub max_quads: u64,
    /// Also read chunks the game had not finished generating (see [`grid::LoadLimits`])
    pub include_unfinished: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_chunks: 4096,
            max_block_bytes: 512 << 20,
            max_quads: 3_000_000,
            include_unfinished: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct BuildOptions {
    pub dimension: String,
    pub window: Window,
    pub limits: Limits,
}

impl BuildOptions {
    pub fn new(window: Window) -> Self {
        BuildOptions {
            dimension: "overworld".into(),
            window,
            limits: Limits::default(),
        }
    }
}

/// One texture with one tint, as it is drawn
#[derive(Clone, Debug)]
pub struct WorldMaterial {
    pub texture: Arc<str>,
    pub tint: Option<[u8; 3]>,
    pub alpha: AlphaClass,
    /// The texture's first frame with the tint multiplied in
    pub image: Arc<Rgba>,
}

impl WorldMaterial {
    pub fn name(&self) -> String {
        match self.tint {
            Some(c) => format!("{}#{:02x}{:02x}{:02x}", self.texture, c[0], c[1], c[2]),
            None => self.texture.to_string(),
        }
    }
}

/// Quads of one material in one chunk column. Quad `i` is vertices `4i..4i+4`, counter-clockwise
/// from outside, two triangles `(0 1 2)` and `(0 2 3)`; the normal is per quad.
#[derive(Clone, Debug, Default)]
pub struct Group {
    pub material: u32,
    pub positions: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub normals: Vec<[f32; 3]>,
}

impl Group {
    pub fn quads(&self) -> usize {
        self.normals.len()
    }
}

#[derive(Clone, Debug)]
pub struct ChunkMesh {
    pub cx: i32,
    pub cz: i32,
    pub groups: Vec<Group>,
}

impl ChunkMesh {
    pub fn quads(&self) -> usize {
        self.groups.iter().map(Group::quads).sum()
    }
}

#[derive(Clone, Debug, Default)]
pub struct WorldMesh {
    pub name: String,
    pub materials: Vec<WorldMaterial>,
    pub chunks: Vec<ChunkMesh>,
    pub spawn: Option<[i32; 3]>,
}

impl WorldMesh {
    pub fn quad_count(&self) -> u64 {
        self.chunks.iter().map(|c| c.quads() as u64).sum()
    }
}

/// A block or a kind of block that is in the world and not in the scene
#[derive(Clone, Debug, PartialEq)]
pub struct LeftOutBlock {
    pub name: String,
    pub reason: String,
    pub blocks: u64,
    /// One state of it that was met, `props` as `a=b,c=d`
    pub example: String,
}

#[derive(Clone, Debug, Default)]
pub struct WorldReport {
    pub world: String,
    pub dimension: String,
    pub game_version: Option<String>,
    pub jar_version: String,
    /// Said when the world was saved by a different game version than the jar's
    pub version_note: Option<String>,
    pub region: (i32, i32),
    pub radius: u32,
    /// Chunk window, inclusive
    pub chunk_window: (i32, i32, i32, i32),
    pub y: Option<(i32, i32)>,
    pub load: LoadStats,
    /// Blocks in the window that are not air
    pub blocks: u64,
    /// Of them, blocks that have geometry
    pub blocks_drawn: u64,
    pub quads: u64,
    /// Faces dropped because the neighbour covers them
    pub culled: u64,
    pub materials: usize,
    pub left_out: Vec<LeftOutBlock>,
    /// Blocks the jar has no blockstate for (another version, a mod)
    pub unknown: Vec<LeftOutBlock>,
    /// Blocks with `waterlogged=true`: the block is drawn (if it has geometry), its water is not
    pub waterlogged: u64,
    /// Blocks whose faces have a colour from a biome colormap
    pub tinted: u64,
    /// Blocks with a `tintindex` whose colour is not in the data nor in the table (name, blocks)
    pub untinted: Vec<(String, u64)>,
    /// Blocks in the table whose biome has no colour (no biome data in the jar, or numeric biomes)
    pub untinted_no_biome: u64,
    /// Biomes met that the game colours by a rule in code, and what it says
    pub biome_modifiers: BTreeMap<String, String>,
    pub unknown_biomes: Vec<String>,
    pub notes: Vec<String>,
}

impl WorldReport {
    /// The report as lines of text for a person
    pub fn lines(&self) -> Vec<String> {
        let esc = crate::jar::escape_control;
        let mut v = Vec::new();
        v.push(format!(
            "world {} ({}), saved by Minecraft {}, read with the jar of {}",
            esc(&self.world),
            esc(&self.dimension),
            self.game_version
                .as_deref()
                .map(esc)
                .unwrap_or_else(|| "?".into()),
            esc(&self.jar_version)
        ));
        if let Some(n) = &self.version_note {
            v.push(format!("  note: {}", esc(n)));
        }
        let (a, b, c, d) = self.chunk_window;
        v.push(format!(
            "window: region {},{} radius {}, chunks {a}..{c} x {b}..{d}{}",
            self.region.0,
            self.region.1,
            self.radius,
            self.y
                .map(|(l, h)| format!(", y {l}..{h}"))
                .unwrap_or_default()
        ));
        v.push(format!(
            "chunks: {} listed, {} read (+{} of the border)",
            self.load.listed, self.load.loaded, self.load.apron_loaded
        ));
        for (s, n) in &self.load.unfinished {
            v.push(if self.load.unfinished_included {
                format!(
                    "  {n} chunk(s) not finished by the game (status `{}`): read as saved, which is not what the game would show",
                    esc(s)
                )
            } else {
                format!(
                    "  {n} chunk(s) not finished by the game (status `{}`): left out, the game regenerates them",
                    esc(s)
                )
            });
        }
        if self.load.legacy > 0 {
            v.push(format!(
                "  {} chunk(s) with numeric block ids (before 1.13, data versions {:?}): left out; the id table and each block's data-value meaning are in the game's code",
                self.load.legacy, self.load.legacy_versions
            ));
        }
        if self.load.failed_count > 0 {
            v.push(format!(
                "  {} chunk(s) failed to read:",
                self.load.failed_count
            ));
            for (x, z, e) in &self.load.failed {
                v.push(format!("    chunk {x},{z}: {}", esc(e)));
            }
        }
        if self.load.apron_failed > 0 {
            v.push(format!(
                "  {} chunk(s) of the border failed to read (their sides are drawn as if open)",
                self.load.apron_failed
            ));
        }
        v.push(format!(
            "blocks: {} not air, {} drawn, {} quads kept, {} faces dropped as hidden, {} materials",
            self.blocks, self.blocks_drawn, self.quads, self.culled, self.materials
        ));
        if !self.left_out.is_empty() {
            v.push("left out (the data has no geometry for these):".into());
            for l in &self.left_out {
                v.push(format!(
                    "  {} x{} [{}]: {}",
                    esc(&l.name),
                    l.blocks,
                    esc(&l.example),
                    esc(&l.reason)
                ));
            }
        }
        if !self.unknown.is_empty() {
            v.push("not in the jar:".into());
            for l in &self.unknown {
                v.push(format!(
                    "  {} x{}: {}",
                    esc(&l.name),
                    l.blocks,
                    esc(&l.reason)
                ));
            }
        }
        if self.waterlogged > 0 {
            v.push(format!(
                "{} waterlogged block(s): the block is drawn, its water is left out (a fluid, drawn by code)",
                self.waterlogged
            ));
        }
        v.push(format!(
            "tinted by a biome colormap: {} block(s)",
            self.tinted
        ));
        for (n, c) in self.untinted.iter().take(LIST_LINES) {
            v.push(format!(
                "  {} x{}: has a tintindex and no colour source in the data or the table: drawn untinted",
                esc(n),
                c
            ));
        }
        if self.untinted_no_biome > 0 {
            v.push(format!(
                "  {} block(s) left untinted: no biome colour for them",
                self.untinted_no_biome
            ));
        }
        for (b, m) in self.biome_modifiers.iter().take(LIST_LINES) {
            v.push(format!(
                "  biome {}: {} is applied by the game's code and not here",
                esc(b),
                esc(m)
            ));
        }
        if !self.unknown_biomes.is_empty() {
            v.push(format!(
                "  biomes the jar has no data for: {}",
                esc(&self.unknown_biomes.join(", "))
            ));
        }
        if self.load.numeric_biomes > 0 {
            v.push(format!(
                "  {} chunk(s) store biomes as numbers (before 1.18): which biome a number is lives in the game's code, so those chunks are untinted",
                self.load.numeric_biomes
            ));
        }
        for n in &self.notes {
            v.push(format!("note: {}", esc(n)));
        }
        v
    }
}

/// Lines of any one list in [`WorldReport::lines`]; the report itself holds all of them
const LIST_LINES: usize = 60;

const DIRS: [[i32; 3]; 6] = [
    [0, -1, 0],
    [0, 1, 0],
    [0, 0, -1],
    [0, 0, 1],
    [-1, 0, 0],
    [1, 0, 0],
];

#[derive(Hash, PartialEq, Eq)]
struct MatKey {
    tex: u32,
    translucent: bool,
    tint: Option<[u8; 3]>,
}

fn same_version(level: &LevelInfo, assets: &Assets) -> Option<String> {
    let jar = assets.version();
    let jar_dv = jar.json.as_ref().and_then(|j| j.world_version);
    let world_name = level.version_name.as_deref();
    match (level.data_version, jar_dv) {
        (Some(w), Some(j)) if w == j => None,
        (Some(w), Some(j)) if w < j => Some(format!(
            "the world was saved by an older game (data version {w}, version {}) than the jar's ({j}, {}): blocks renamed since then are left out as not in the jar",
            world_name.unwrap_or("?"),
            jar.id
        )),
        (Some(w), Some(j)) => Some(format!(
            "the world was saved by a newer game (data version {w}, version {}) than the jar's ({j}, {}): blocks added since then are left out as not in the jar",
            world_name.unwrap_or("?"),
            jar.id
        )),
        _ => match world_name {
            Some(n) if n == jar.id => None,
            Some(n) => Some(format!(
                "the world says Minecraft {n}, the jar is {}; the data versions are not both known, so blocks that differ between them are left out as not in the jar",
                jar.id
            )),
            None => Some(
                "the save has no version (before 1.9) and so cannot be matched to the jar".into(),
            ),
        },
    }
}

/// Builds the meshes of a window of a world
pub fn build(
    assets: &Assets,
    world: &World,
    opts: &BuildOptions,
) -> Result<(WorldMesh, WorldReport)> {
    assets.require_models()?;
    let region_dir = world.region_dir(&opts.dimension)?;
    let win = opts.window.chunk_range()?;
    let mut table = StateTable::new(assets);
    let mut biomes = BiomeTable::new(assets);
    let (grid, load) = grid::load(
        &region_dir,
        win,
        &mut table,
        &mut biomes,
        &LoadLimits {
            max_chunks: opts.limits.max_chunks,
            max_block_bytes: opts.limits.max_block_bytes,
            include_unfinished: opts.limits.include_unfinished,
        },
    )?;
    let mut m = Mesher {
        table: &table,
        biomes: &biomes,
        grid: &grid,
        bank: Vec::new(),
        bank_index: HashMap::new(),
        counts: vec![0; table.states.len()],
        tinted_blocks: 0,
        untinted_by_name: BTreeMap::new(),
        untinted_no_biome: 0,
        culled: 0,
        quads: 0,
        y: opts.window.y,
        max_quads: opts.limits.max_quads,
    };
    let mut chunks = Vec::new();
    for cz in win.1..=win.3 {
        for cx in win.0..=win.2 {
            if grid.chunk(cx, cz).is_none() {
                continue;
            }
            let cm = m.chunk(cx, cz)?;
            if !cm.groups.is_empty() {
                chunks.push(cm);
            }
        }
    }
    // report
    let mut report = WorldReport {
        world: world.name(),
        dimension: opts.dimension.clone(),
        game_version: world.level.version_name.clone(),
        jar_version: assets.version().id.clone(),
        version_note: same_version(&world.level, assets),
        region: opts.window.region,
        radius: opts.window.radius,
        chunk_window: win,
        y: opts.window.y,
        load,
        quads: m.quads,
        culled: m.culled,
        materials: m.bank.len(),
        tinted: m.tinted_blocks,
        untinted: m
            .untinted_by_name
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect(),
        untinted_no_biome: m.untinted_no_biome,
        biome_modifiers: biomes.modifiers.clone(),
        unknown_biomes: biomes.unknown.iter().cloned().collect(),
        ..WorldReport::default()
    };
    let mut lo: BTreeMap<(String, String), LeftOutBlock> = BTreeMap::new();
    let mut unk: BTreeMap<(String, String), LeftOutBlock> = BTreeMap::new();
    for (id, &n) in m.counts.iter().enumerate() {
        if n == 0 || id as u16 == AIR {
            continue;
        }
        let st = &table.states[id];
        report.blocks += n;
        if st.waterlogged {
            report.waterlogged += n;
        }
        let (map, reason) = match &st.fate {
            Fate::Drawn => {
                report.blocks_drawn += n;
                continue;
            }
            Fate::Air => continue,
            Fate::LeftOut(r) => (&mut lo, r.clone()),
            Fate::Unknown(r) => (&mut unk, r.clone()),
        };
        let e = map
            .entry((st.name.to_string(), reason.clone()))
            .or_insert_with(|| LeftOutBlock {
                name: st.name.to_string(),
                reason,
                blocks: 0,
                example: st.props.clone(),
            });
        e.blocks += n;
    }
    let sort = |m: BTreeMap<(String, String), LeftOutBlock>| {
        let mut v: Vec<LeftOutBlock> = m.into_values().collect();
        v.sort_by(|a, b| b.blocks.cmp(&a.blocks).then(a.name.cmp(&b.name)));
        v
    };
    report.left_out = sort(lo);
    report.unknown = sort(unk);
    if assets.biome_names().is_empty() {
        report
            .notes
            .push("this jar has no biome data (`data/minecraft/worldgen/biome`: the 1.19.2 jar here has none, 1.20.1 and later do), so nothing is tinted by biome".into());
    }
    let mesh = WorldMesh {
        name: world.name(),
        materials: m.bank,
        chunks,
        spawn: world.level.spawn,
    };
    Ok((mesh, report))
}

struct Mesher<'a> {
    table: &'a StateTable<'a>,
    biomes: &'a BiomeTable<'a>,
    grid: &'a BlockGrid,
    bank: Vec<WorldMaterial>,
    bank_index: HashMap<MatKey, u32>,
    counts: Vec<u64>,
    tinted_blocks: u64,
    untinted_by_name: BTreeMap<String, u64>,
    untinted_no_biome: u64,
    culled: u64,
    quads: u64,
    y: Option<(i32, i32)>,
    max_quads: u64,
}

impl Mesher<'_> {
    fn material(&mut self, tex: u32, translucent: bool, tint: Option<[u8; 3]>) -> u32 {
        let key = MatKey {
            tex,
            translucent,
            tint,
        };
        if let Some(&i) = self.bank_index.get(&key) {
            return i;
        }
        let t = &self.table.textures[tex as usize];
        let image = match tint {
            Some(c) => Arc::new(t.image.tinted(c)),
            None => t.image.clone(),
        };
        let alpha = t.image.alpha_class(translucent);
        self.bank.push(WorldMaterial {
            texture: t.path.clone(),
            tint,
            alpha,
            image,
        });
        let i = (self.bank.len() - 1) as u32;
        self.bank_index.insert(key, i);
        i
    }

    fn chunk(&mut self, cx: i32, cz: i32) -> Result<ChunkMesh> {
        let grid = self.grid;
        let table = self.table;
        let Some(cb) = grid.chunk(cx, cz) else {
            return Ok(ChunkMesh {
                cx,
                cz,
                groups: Vec::new(),
            });
        };
        let mut groups: Vec<Group> = Vec::new();
        let mut group_of: HashMap<u32, usize> = HashMap::new();
        for (si, sec) in cb.secs.iter().enumerate() {
            let base_y = (cb.min_sec + si as i32) * 16;
            if let Sec::Uniform(AIR) = sec {
                continue;
            }
            if let Some((lo, hi)) = self.y {
                if base_y + 15 < lo || base_y > hi {
                    continue;
                }
            }
            for ly in 0..16i32 {
                let y = base_y + ly;
                if let Some((lo, hi)) = self.y {
                    if y < lo || y > hi {
                        continue;
                    }
                }
                for lz in 0..16i32 {
                    for lx in 0..16i32 {
                        let id = match sec {
                            Sec::Uniform(v) => *v,
                            Sec::Dense(d) => d[((ly << 8) | (lz << 4) | lx) as usize],
                        };
                        if id == AIR {
                            continue;
                        }
                        self.counts[id as usize] += 1;
                        let st = &table.states[id as usize];
                        if !matches!(st.fate, Fate::Drawn) {
                            continue;
                        }
                        let (wx, wz) = (cx * 16 + lx, cz * 16 + lz);
                        // the state of the block on a side, fetched when a face asks for it
                        let mut nb = [u16::MAX; 6];
                        // the colour of this block's tinted faces
                        let mut tint: Option<Option<[u8; 3]>> = None;
                        for q in &st.quads {
                            if let Some(d) = q.cullface {
                                let di = dir_index(d);
                                if nb[di] == u16::MAX {
                                    let [dx, dy, dz] = DIRS[di];
                                    let (nx, ny, nz) = (lx + dx, ly + dy, lz + dz);
                                    nb[di] = if (0..16).contains(&nx)
                                        && (0..16).contains(&ny)
                                        && (0..16).contains(&nz)
                                    {
                                        match sec {
                                            Sec::Uniform(v) => *v,
                                            Sec::Dense(dn) => {
                                                dn[((ny << 8) | (nz << 4) | nx) as usize]
                                            }
                                        }
                                    } else {
                                        grid.get(wx + dx, y + dy, wz + dz)
                                    };
                                }
                                if table.states[nb[di] as usize].covers[di ^ 1] {
                                    self.culled += 1;
                                    continue;
                                }
                            }
                            let mut colour = None;
                            if q.tint_index.is_some() {
                                let t = *tint.get_or_insert_with(|| match st.tint {
                                    None => {
                                        *self
                                            .untinted_by_name
                                            .entry(st.name.to_string())
                                            .or_default() += 1;
                                        None
                                    }
                                    Some(k) => {
                                        let b = grid.biome(wx, y, wz);
                                        match self.biomes.color(b, k) {
                                            Some(c) => {
                                                self.tinted_blocks += 1;
                                                Some(c)
                                            }
                                            None => {
                                                self.untinted_no_biome += 1;
                                                None
                                            }
                                        }
                                    }
                                });
                                colour = t;
                            }
                            let mat = self.material(q.tex, q.translucent, colour);
                            let gi = *group_of.entry(mat).or_insert_with(|| {
                                groups.push(Group {
                                    material: mat,
                                    ..Group::default()
                                });
                                groups.len() - 1
                            });
                            let g = &mut groups[gi];
                            for k in 0..4 {
                                g.positions.push([
                                    lx as f32 + q.pos[k][0],
                                    y as f32 + q.pos[k][1],
                                    lz as f32 + q.pos[k][2],
                                ]);
                                g.uvs.push(q.uv[k]);
                            }
                            g.normals.push(q.normal);
                            self.quads += 1;
                        }
                    }
                }
            }
        }
        if self.quads > self.max_quads {
            bail!(
                "the scene has more than {} quads (at chunk {cx},{cz}); use a smaller window (--chunks, --y, or a smaller --radius)",
                self.max_quads
            );
        }
        // vectors grown by doubling hold up to twice what they need; the whole scene is held at once
        for g in &mut groups {
            g.positions.shrink_to_fit();
            g.uvs.shrink_to_fit();
            g.normals.shrink_to_fit();
        }
        Ok(ChunkMesh { cx, cz, groups })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tint::ColormapKind;
    use crate::world::testworld::*;

    fn run(chunks: &[ChunkSpec], win: Window) -> (WorldMesh, WorldReport) {
        let save = write_world("mesher", "9.9", 9000, chunks, false);
        let a = world_jar();
        let w = World::open(&save).unwrap();
        let r = build(&a, &w, &BuildOptions::new(win));
        cleanup(&save);
        r.unwrap()
    }

    fn one_chunk(c: &ChunkSpec) -> (WorldMesh, WorldReport) {
        run(std::slice::from_ref(c), Window::region(0, 0))
    }

    #[test]
    fn windows() {
        let w = Window::region(0, 0);
        assert_eq!(w.chunk_range().unwrap(), (0, 0, 31, 31));
        let w = Window::region(-1, 2);
        assert_eq!(w.chunk_range().unwrap(), (-32, 64, -1, 95));
        let mut w = Window::region(0, 0);
        w.radius = 1;
        assert_eq!(w.chunk_range().unwrap(), (-32, -32, 63, 63));
        w.chunks = Some([0, 0, 3, 3]);
        assert_eq!(w.chunk_range().unwrap(), (0, 0, 3, 3));
        w.chunks = Some([100, 100, 103, 103]);
        assert!(w.chunk_range().is_err());
        w.chunks = Some([3, 3, 0, 0]);
        assert!(w.chunk_range().is_err());
        w.chunks = None;
        w.radius = MAX_RADIUS + 1;
        assert!(w.chunk_range().is_err());
        let mut w = Window::region(i32::MAX, 0);
        assert!(w.chunk_range().is_err());
        w.region = (0, 0);
        w.y = Some((5, 4));
        assert!(w.chunk_range().is_err());
    }

    #[test]
    fn adjacent_cubes_drop_the_faces_between_them() {
        let mut c = ChunkSpec::new(0, 0);
        c.fill((1, 64, 1), (2, 64, 1), "minecraft:stone");
        let (m, r) = one_chunk(&c);
        assert_eq!(m.quad_count(), 10);
        assert_eq!(r.culled, 2);
        assert_eq!(r.blocks, 2);
        assert_eq!(r.blocks_drawn, 2);
        // a lone cube keeps all six
        let mut c = ChunkSpec::new(0, 0);
        c.set(1, 64, 1, "minecraft:stone", &[]);
        assert_eq!(one_chunk(&c).0.quad_count(), 6);
        // a solid 3 x 3 x 3 shows only its skin: 6 sides of 9
        let mut c = ChunkSpec::new(0, 0);
        c.fill((1, 64, 1), (3, 66, 3), "minecraft:stone");
        assert_eq!(one_chunk(&c).0.quad_count(), 54);
    }

    #[test]
    fn stone_hides_the_face_of_glass_that_looks_at_it_and_glass_hides_nothing() {
        let mut c = ChunkSpec::new(0, 0);
        c.set(1, 64, 1, "minecraft:stone", &[]);
        c.set(2, 64, 1, "minecraft:glass", &[]);
        let (m, r) = one_chunk(&c);
        // the stone keeps its east face, the glass loses its west one
        assert_eq!(m.quad_count(), 6 + 5);
        assert_eq!(r.culled, 1);
        // the glass texture has alpha: its material is not opaque
        let g = m
            .materials
            .iter()
            .find(|x| &*x.texture == "block/glass")
            .unwrap();
        assert_ne!(g.alpha, AlphaClass::Opaque);
    }

    #[test]
    fn a_slab_covers_only_the_side_that_is_on_the_cell_boundary() {
        // a bottom slab under stone: its top is at half height, so the stone's bottom face stays
        let mut c = ChunkSpec::new(0, 0);
        c.set(1, 64, 1, "minecraft:oak_slab", &[("type", "bottom")]);
        c.set(1, 65, 1, "minecraft:stone", &[]);
        let (m, r) = one_chunk(&c);
        // slab: 6 quads (its down face has a cullface and air below: kept); stone: 6 (bottom kept)
        assert_eq!(m.quad_count(), 12);
        assert_eq!(r.culled, 0);
        // a top slab under stone: its top is on the boundary and covers it, the stone's bottom goes,
        // and the slab's own top (cullface up) goes against the stone
        let mut c = ChunkSpec::new(0, 0);
        c.set(1, 64, 1, "minecraft:oak_slab", &[("type", "top")]);
        c.set(1, 65, 1, "minecraft:stone", &[]);
        let (m, r) = one_chunk(&c);
        assert_eq!(m.quad_count(), 10);
        assert_eq!(r.culled, 2);
    }

    #[test]
    fn culling_reaches_across_chunk_borders_and_into_the_border_chunks() {
        let mut a = ChunkSpec::new(0, 0);
        a.set(15, 64, 1, "minecraft:stone", &[]);
        let mut b = ChunkSpec::new(1, 0);
        b.set(16, 64, 1, "minecraft:stone", &[]);
        // both chunks in the window: both faces go
        let mut w = Window::region(0, 0);
        w.chunks = Some([0, 0, 1, 0]);
        let (m, _) = run(&[a.clone(), b.clone()], w.clone());
        assert_eq!(m.quad_count(), 10);
        // only chunk 0 in the window: chunk 1 is read as its border and still hides the face
        w.chunks = Some([0, 0, 0, 0]);
        let (m, r) = run(&[a.clone(), b.clone()], w.clone());
        assert_eq!(m.quad_count(), 5);
        assert_eq!(r.load.apron_loaded, 1);
        assert_eq!(r.load.loaded, 1);
        // no neighbour chunk: nothing to hide it
        let (m, _) = run(&[a.clone()], w.clone());
        assert_eq!(m.quad_count(), 6);
        // a neighbour the game had not finished is not a neighbour
        let mut b2 = b.clone();
        b2.status = Some("minecraft:features".into());
        let (m, _) = run(&[a, b2], w);
        assert_eq!(m.quad_count(), 6);
    }

    #[test]
    fn culling_reaches_across_sections_and_below_zero() {
        // sections -5 and -4 (y -65 and -64), and -1 and 0 (y -1 and 0)
        for (lo, hi) in [(-65, -64), (-1, 0), (15, 16)] {
            let mut c = ChunkSpec::new(0, 0);
            c.set(3, lo, 3, "minecraft:stone", &[]);
            c.set(3, hi, 3, "minecraft:stone", &[]);
            let (m, r) = one_chunk(&c);
            assert_eq!(m.quad_count(), 10, "{lo}/{hi}");
            assert_eq!(r.culled, 2);
        }
        // and the block really sits at the negative height
        let mut c = ChunkSpec::new(0, 0);
        c.set(3, -64, 3, "minecraft:stone", &[]);
        let (m, _) = one_chunk(&c);
        let lo = m.chunks[0].groups[0]
            .positions
            .iter()
            .map(|p| p[1])
            .fold(f32::MAX, f32::min);
        assert_eq!(lo, -64.0);
    }

    #[test]
    fn positions_are_the_games_block_coordinates() {
        let mut c = ChunkSpec::new(-1, 2);
        // chunk -1,2 covers x -16..-1, z 32..47; this block is at (-3, 70, 40)
        c.set(-3, 70, 40, "minecraft:stone", &[]);
        let mut w = Window::region(-1, 0);
        w.chunks = Some([-1, 2, -1, 2]);
        let (m, _) = run(&[c], w);
        let ch = &m.chunks[0];
        assert_eq!((ch.cx, ch.cz), (-1, 2));
        // chunk-local x is 13 (= -3 - (-16)), and the node adds -16
        let xs: Vec<f32> = ch.groups[0].positions.iter().map(|p| p[0]).collect();
        assert_eq!(xs.iter().cloned().fold(f32::MAX, f32::min), 13.0);
        assert_eq!(xs.iter().cloned().fold(f32::MIN, f32::max), 14.0);
        let zs: Vec<f32> = ch.groups[0].positions.iter().map(|p| p[2]).collect();
        assert_eq!(zs.iter().cloned().fold(f32::MAX, f32::min), 8.0);
        assert_eq!(m.spawn, Some([8, 70, 8]));
    }

    fn colour(a: &Assets, kind: ColormapKind, biome: &str) -> [u8; 3] {
        let (t, _) = a.tint_for_biome(kind, biome).unwrap();
        a.tint_rgb(&t).unwrap()
    }

    #[test]
    fn tints_come_from_the_biome_of_each_cell_through_the_jars_colormaps() {
        let a = world_jar();
        let mut c = ChunkSpec::new(0, 0);
        c.set(1, 64, 1, "minecraft:grass_block", &[("snowy", "false")]);
        // x = 4..7 is biome cell 1 of section 4
        c.set(4, 64, 1, "minecraft:grass_block", &[("snowy", "false")]);
        c.biome_cells.insert((4, 1), "minecraft:desert".into());
        c.set(1, 64, 8, "minecraft:oak_leaves", &[]);
        let (m, r) = one_chunk(&c);
        let plains = colour(&a, ColormapKind::Grass, "plains");
        let desert = colour(&a, ColormapKind::Grass, "desert");
        assert_ne!(plains, desert);
        let tints: Vec<Option<[u8; 3]>> = m
            .materials
            .iter()
            .filter(|x| &*x.texture == "block/grass_block")
            .map(|x| x.tint)
            .collect();
        assert!(tints.contains(&Some(plains)), "{tints:?}");
        assert!(tints.contains(&Some(desert)), "{tints:?}");
        // the colour is multiplied into the texels: grey 200 times the colour / 255
        let g = m
            .materials
            .iter()
            .find(|x| x.tint == Some(plains) && &*x.texture == "block/grass_block")
            .unwrap();
        let px = g.image.pixel(0, 0);
        assert_eq!(px[0], ((200 * plains[0] as u32 + 127) / 255) as u8);
        assert_eq!(px[3], 255);
        // leaves take the foliage colormap
        let foliage = colour(&a, ColormapKind::Foliage, "plains");
        assert!(m
            .materials
            .iter()
            .any(|x| &*x.texture == "block/leaf" && x.tint == Some(foliage)));
        // only the faces with a tintindex are tinted: one grass face and all of the leaf's
        assert_eq!(r.tinted, 3);
        assert_eq!(r.untinted, vec![]);
    }

    #[test]
    fn biome_data_overrides_and_code_rules_are_reported() {
        let mut c = ChunkSpec::new(0, 0);
        c.biome = Some("minecraft:swamp".into());
        c.set(1, 64, 1, "minecraft:oak_leaves", &[]);
        c.set(3, 64, 1, "minecraft:grass_block", &[("snowy", "false")]);
        c.set(5, 64, 1, "minecraft:birch_leaves", &[]);
        let (m, r) = one_chunk(&c);
        // the swamp's own foliage colour
        assert!(m
            .materials
            .iter()
            .any(|x| x.tint == Some([0x6a, 0x70, 0x39])));
        // its grass modifier is the game's code
        assert!(r.biome_modifiers["minecraft:swamp"].contains("grass_color_modifier swamp"));
        // birch leaves have a tintindex and are not in the table
        assert_eq!(r.untinted, vec![("minecraft:birch_leaves".to_string(), 1)]);
        let text = r.lines().join("\n");
        assert!(
            text.contains("birch_leaves") && text.contains("swamp"),
            "{text}"
        );
    }

    #[test]
    fn what_has_no_geometry_or_is_not_in_the_jar_is_left_out_and_counted() {
        let mut c = ChunkSpec::new(0, 0);
        c.fill((0, 64, 0), (0, 64, 2), "minecraft:water");
        c.set(2, 64, 0, "minecraft:chest", &[]);
        c.set(3, 64, 0, "minecraft:no_such_block", &[]);
        c.set(4, 64, 0, "somemod:thing", &[]);
        c.set(
            5,
            64,
            0,
            "minecraft:oak_slab",
            &[("type", "bottom"), ("waterlogged", "true")],
        );
        c.set(6, 64, 0, "minecraft:air", &[]);
        c.set(7, 64, 0, "minecraft:cave_air", &[]);
        let (m, r) = one_chunk(&c);
        assert_eq!(r.blocks, 3 + 1 + 1 + 1 + 1, "air is not counted");
        assert_eq!(r.blocks_drawn, 1);
        assert_eq!(m.quad_count(), 6);
        let by = |n: &str| r.left_out.iter().find(|l| l.name == n).unwrap();
        assert_eq!(by("minecraft:water").blocks, 3);
        assert!(by("minecraft:water").reason.contains("fluid"));
        assert_eq!(by("minecraft:chest").blocks, 1);
        assert_eq!(r.unknown.len(), 2);
        assert!(r
            .unknown
            .iter()
            .any(|u| u.name == "minecraft:no_such_block" && u.reason.contains("no blockstate")));
        assert!(r
            .unknown
            .iter()
            .any(|u| u.name == "somemod:thing" && u.reason.contains("namespace")));
        assert_eq!(r.waterlogged, 1);
        let text = r.lines().join("\n");
        assert!(
            text.contains("waterlogged") && text.contains("minecraft:chest"),
            "{text}"
        );
    }

    #[test]
    fn a_state_the_blockstate_has_no_entry_for_is_left_out_with_the_reason() {
        let mut c = ChunkSpec::new(0, 0);
        c.set(1, 64, 1, "minecraft:oak_slab", &[("type", "sideways")]);
        let (m, r) = one_chunk(&c);
        assert_eq!(m.quad_count(), 0);
        assert_eq!(r.left_out.len(), 1);
        assert!(
            r.left_out[0].reason.contains("could not be baked"),
            "{:?}",
            r.left_out
        );
    }

    #[test]
    fn unfinished_legacy_and_damaged_chunks_are_counted_not_fatal() {
        use crate::world::nbt::write::*;
        use crate::world::nbt::Tag;
        let mut ok = ChunkSpec::new(0, 0);
        ok.set(1, 64, 1, "minecraft:stone", &[]);
        let mut part = ChunkSpec::new(1, 0);
        part.set(17, 64, 1, "minecraft:stone", &[]);
        part.status = Some("minecraft:noise".into());
        let legacy = document(
            "",
            &compound(vec![(
                "Level",
                compound(vec![
                    ("xPos", Tag::Int(2)),
                    ("zPos", Tag::Int(0)),
                    (
                        "Sections",
                        Tag::List(vec![compound(vec![
                            ("Y", Tag::Byte(0)),
                            ("Blocks", Tag::ByteArray(vec![1; 4096])),
                        ])]),
                    ),
                ]),
            )]),
        );
        let wrong_place = document("", &{
            let mut c = ChunkSpec::new(9, 9);
            c.set(150, 64, 150, "minecraft:stone", &[]);
            c.nbt()
        });
        let raw = vec![
            (2, 0, legacy),
            (3, 0, b"this is not nbt".to_vec()),
            (4, 0, wrong_place),
        ];
        let save = write_world_with("mesher-bad", "9.9", 9000, &[ok, part], &raw, false);
        let a = world_jar();
        let w = World::open(&save).unwrap();
        let (m, r) = build(&a, &w, &BuildOptions::new(Window::region(0, 0))).unwrap();
        assert_eq!(
            m.quad_count(),
            6,
            "the good chunk is read, and nothing else"
        );
        assert_eq!(r.load.listed, 5);
        assert_eq!(r.load.loaded, 1);
        assert_eq!(r.load.unfinished.get("minecraft:noise"), Some(&1));
        assert_eq!(r.load.legacy, 1);
        assert_eq!(r.load.failed_count, 2, "{:?}", r.load.failed);
        let text = r.lines().join("\n");
        assert!(
            text.contains("numeric block ids") && text.contains("failed to read"),
            "{text}"
        );
        // asked for, the unfinished chunk is read as saved
        let mut o = BuildOptions::new(Window::region(0, 0));
        o.limits.include_unfinished = true;
        let (m, r) = build(&a, &w, &o).unwrap();
        assert_eq!(m.quad_count(), 12);
        assert!(r.lines().join("\n").contains("read as saved"));
        cleanup(&save);
    }

    #[test]
    fn the_height_window_keeps_the_hidden_faces_hidden() {
        let mut c = ChunkSpec::new(0, 0);
        c.fill((1, 64, 1), (1, 80, 1), "minecraft:stone");
        let mut w = Window::region(0, 0);
        w.y = Some((70, 72));
        let (m, r) = run(&[c], w);
        // three cubes in a column: only their sides; the faces at the cut are hidden by the blocks
        // above and below the window, which are in the world
        assert_eq!(m.quad_count(), 3 * 4);
        assert_eq!(r.blocks, 3);
    }

    #[test]
    fn a_scene_over_the_quad_budget_is_refused() {
        let mut c = ChunkSpec::new(0, 0);
        c.fill((0, 64, 0), (5, 64, 5), "minecraft:stone");
        let save = write_world("mesher-budget", "9.9", 9000, &[c], false);
        let a = world_jar();
        let w = World::open(&save).unwrap();
        let mut o = BuildOptions::new(Window::region(0, 0));
        o.limits.max_quads = 10;
        let e = build(&a, &w, &o).unwrap_err().to_string();
        assert!(e.contains("more than 10 quads"), "{e}");
        let mut o = BuildOptions::new(Window::region(0, 0));
        o.limits.max_chunks = 0;
        let e = build(&a, &w, &o).unwrap_err().to_string();
        assert!(e.contains("lists 1 chunks"), "{e}");
        let mut o = BuildOptions::new(Window::region(0, 0));
        o.limits.max_block_bytes = 10;
        assert!(build(&a, &w, &o).is_err());
        cleanup(&save);
    }

    #[test]
    fn the_1_13_layout_and_the_pre_1_16_folder_read_the_same_way() {
        let mut c = ChunkSpec::new(0, 0);
        c.fill((1, 64, 1), (3, 64, 3), "minecraft:stone");
        c.set(8, 64, 8, "minecraft:oak_slab", &[("type", "bottom")]);
        let modern = one_chunk(&c).0;
        let mut old = c.clone();
        old.old_layout = true;
        let save = write_world("mesher-old", "9.9", 1519, &[old], true);
        let a = world_jar();
        let w = World::open(&save).unwrap();
        assert!(w.region_dir("overworld").unwrap().ends_with("region"));
        let (m, r) = build(&a, &w, &BuildOptions::new(Window::region(0, 0))).unwrap();
        cleanup(&save);
        assert_eq!(m.quad_count(), modern.quad_count());
        assert_eq!(
            r.load.numeric_biomes, 1,
            "1.13 to 1.17 chunks keep numeric biome ids: not read"
        );
        // so nothing is tinted by biome there, and the report says so
        assert!(r.lines().join("\n").contains("store biomes as numbers"));
    }

    #[test]
    fn a_radius_adds_the_regions_around() {
        let mut a = ChunkSpec::new(0, 0);
        a.set(1, 64, 1, "minecraft:stone", &[]);
        let mut b = ChunkSpec::new(32, 0);
        b.set(33 * 16 - 16 + 1, 64, 1, "minecraft:stone", &[]);
        let (m, _) = run(&[a.clone(), b.clone()], Window::region(0, 0));
        assert_eq!(m.chunks.len(), 1);
        let mut w = Window::region(0, 0);
        w.radius = 1;
        let (m, r) = run(&[a, b], w);
        assert_eq!(m.chunks.len(), 2);
        assert_eq!(r.load.loaded, 2);
    }

    /// A whole region of 1024 chunks, 24 sections each (100 million blocks): `cargo test --release
    /// -- --ignored stress --nocapture`. Two worlds: solid stone (few faces, all the work is
    /// culling) and noise (every face shows: the quad budget stops it).
    #[test]
    #[ignore]
    fn stress_a_full_region() {
        use crate::world::chunk::{block_bits, tests::pack_padded};
        use crate::world::compress::tests::zlib;
        use crate::world::nbt::write::*;
        use crate::world::nbt::Tag;
        use crate::world::region::tests::{region_bytes, temp_dir};
        for noise in [false, true] {
            let mut raw = Vec::new();
            let mut s = 0x0123_4567_89AB_CDEF_u64;
            for cz in 0..32 {
                for cx in 0..32 {
                    let mut sections = Vec::new();
                    for sy in -4..20 {
                        let pal: Vec<Tag> = [
                            "stone",
                            "glass",
                            "grass_block",
                            "oak_leaves",
                            "air",
                            "water",
                        ]
                        .iter()
                        .map(|n| compound(vec![("Name", string(&format!("minecraft:{n}")))]))
                        .collect();
                        let bits = block_bits(pal.len());
                        let idx: Vec<u16> = (0..4096)
                            .map(|_| {
                                s ^= s >> 12;
                                s ^= s << 25;
                                s ^= s >> 27;
                                if noise {
                                    ((s.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) % 6) as u16
                                } else {
                                    0
                                }
                            })
                            .collect();
                        let bs = if noise {
                            compound(vec![
                                ("palette", Tag::List(pal)),
                                ("data", Tag::LongArray(pack_padded(&idx, bits))),
                            ])
                        } else {
                            compound(vec![("palette", Tag::List(vec![pal[0].clone()]))])
                        };
                        sections.push(compound(vec![
                            ("Y", Tag::Byte(sy as i8)),
                            ("block_states", bs),
                            (
                                "biomes",
                                compound(vec![(
                                    "palette",
                                    Tag::List(vec![string("minecraft:plains")]),
                                )]),
                            ),
                        ]));
                    }
                    let c = compound(vec![
                        ("DataVersion", Tag::Int(9000)),
                        ("xPos", Tag::Int(cx)),
                        ("zPos", Tag::Int(cz)),
                        ("Status", string("minecraft:full")),
                        ("sections", Tag::List(sections)),
                    ]);
                    raw.push((cx as u32, cz as u32, 2u8, zlib(&document("", &c))));
                }
            }
            // MC_UNPACK_STRESS_KEEP=<folder> keeps the two saves there (`noise`, `solid`) to try the CLI on
            let keep = std::env::var_os("MC_UNPACK_STRESS_KEEP").map(std::path::PathBuf::from);
            let dir = match &keep {
                Some(k) => k.join(if noise { "noise" } else { "solid" }),
                None => temp_dir("stress"),
            };
            let save = dir.join("save");
            let rd = save.join("dimensions/minecraft/overworld/region");
            std::fs::create_dir_all(&rd).unwrap();
            // the 8 KiB table holds 255 sectors at most per chunk: these are small enough
            std::fs::write(rd.join("r.0.0.mca"), region_bytes(&raw)).unwrap();
            std::fs::write(
                save.join("level.dat"),
                crate::world::compress::tests::gzip(&document(
                    "",
                    &compound(vec![(
                        "Data",
                        compound(vec![("DataVersion", Tag::Int(9000))]),
                    )]),
                )),
            )
            .unwrap();
            drop(raw);
            let a = world_jar();
            let w = World::open(&save).unwrap();
            let t = std::time::Instant::now();
            let mut o = BuildOptions::new(Window::region(0, 0));
            o.limits.max_chunks = 1024;
            let r = build(&a, &w, &o);
            println!(
                "{}: {:?} in {:.2} s, peak RSS {} MB",
                if noise { "noise" } else { "solid stone" },
                r.as_ref()
                    .map(|(m, r)| (m.quad_count(), r.blocks))
                    .map_err(|e| e.to_string()),
                t.elapsed().as_secs_f64(),
                crate::check::peak_rss() / 1_000_000
            );
            if keep.is_none() {
                std::fs::remove_dir_all(&dir).unwrap();
            }
        }
    }
}
