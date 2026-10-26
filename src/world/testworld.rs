//! Synthetic saves for the tests: a tiny jar with the blocks a world test needs, and a writer that
//! turns a list of blocks into real region files (the same bytes the game's format has). No game data.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::chunk::block_bits;
use super::chunk::tests::{entry, pack_padded, pack_spanning};
use super::compress::tests::{gzip, zlib};
use super::nbt::write::*;
use super::nbt::Tag;
use super::region::tests::{region_bytes, temp_dir};
use crate::assets::Assets;
use crate::jar::testjar::{png, JarBuilder};

/// A jar (1.21.4 and later layout: it has item definitions; with biome data and colormaps) holding:
/// `stone`, `glass` (partly transparent), `grass_block` (tinted, and its item definition says grass),
/// `oak_leaves` and `birch_leaves` (tinted; no item definition says what colours them), `oak_slab` (bottom, top, double), `water` and `chest` (no geometry),
/// `fence` (multipart), and the biomes `plains`, `desert`, `swamp`
pub fn world_jar() -> Assets {
    let tex = |c: u8| png(16, 16, move |_, _| [c, c, c, 255]);
    let glass = png(16, 16, |x, _| {
        if x < 8 {
            [200, 200, 255, 128]
        } else {
            [0, 0, 0, 0]
        }
    });
    let leaf = png(16, 16, |x, _| {
        if x < 8 {
            [0, 200, 0, 255]
        } else {
            [0, 0, 0, 0]
        }
    });
    // a colormap whose colour tells where it was read: (x, y, 7)
    let colormap = |_: ()| png(256, 256, |x, y| [x as u8, y as u8, 7, 255]);
    let cube = |tint: bool, tex: &str| {
        let t = if tint { r#","tintindex":0"# } else { "" };
        let face = |d: &str| format!(r##""{d}":{{"texture":"#all","cullface":"{d}"{t}}}"##);
        format!(
            r##"{{"textures":{{"all":"{tex}"}},"elements":[{{"from":[0,0,0],"to":[16,16,16],"faces":{{{}}}}}]}}"##,
            ["down", "up", "north", "south", "west", "east"]
                .map(face)
                .join(",")
        )
    };
    let slab = |from_y: u32, to_y: u32| {
        // only the bottom of a bottom slab, or the top of a top slab, is on the cell boundary
        let (cull_down, cull_up) = (from_y == 0, to_y == 16);
        let f = |d: &str, c: bool| {
            if c {
                format!(r##""{d}":{{"texture":"#all","cullface":"{d}"}}"##)
            } else {
                format!(r##""{d}":{{"texture":"#all"}}"##)
            }
        };
        format!(
            r##"{{"textures":{{"all":"block/stone"}},"elements":[{{"from":[0,{from_y},0],"to":[16,{to_y},16],"faces":{{{},{},{},{},{},{}}}}}]}}"##,
            f("down", cull_down),
            f("up", cull_up),
            f("north", false),
            f("south", false),
            f("west", false),
            f("east", false)
        )
    };
    let biome = |t: f64, d: f64, extra: &str| {
        format!(
            r##"{{"temperature":{t},"downfall":{d},"has_precipitation":true,"effects":{{"water_color":"#3f76e4"{extra}}}}}"##
        )
    };
    let jar = JarBuilder::new()
        // the one place the data says which block takes a biome colormap: the item's tints
        .add(
            "assets/minecraft/items/grass_block.json",
            r#"{"model":{"type":"minecraft:model","model":"minecraft:block/grass_block","tints":[{"type":"minecraft:grass","temperature":0.5,"downfall":1.0}]}}"#,
        )
        .add(
            "assets/minecraft/items/oak_leaves.json",
            r#"{"model":{"type":"minecraft:model","model":"minecraft:block/oak_leaves","tints":[{"type":"minecraft:constant","value":-12012264}]}}"#,
        )
        .add("assets/minecraft/blockstates/stone.json", r#"{"variants":{"":{"model":"block/stone"}}}"#)
        .add("assets/minecraft/blockstates/glass.json", r#"{"variants":{"":{"model":"block/glass"}}}"#)
        .add("assets/minecraft/blockstates/grass_block.json", r#"{"variants":{"snowy=false":{"model":"block/grass_block"},"snowy=true":{"model":"block/grass_block"}}}"#)
        .add("assets/minecraft/blockstates/oak_leaves.json", r#"{"variants":{"":{"model":"block/oak_leaves"}}}"#)
        .add("assets/minecraft/blockstates/birch_leaves.json", r#"{"variants":{"":{"model":"block/oak_leaves"}}}"#)
        .add(
            "assets/minecraft/blockstates/oak_slab.json",
            r#"{"variants":{"type=bottom":{"model":"block/slab_bottom"},"type=top":{"model":"block/slab_top"},"type=double":{"model":"block/stone"}}}"#,
        )
        .add("assets/minecraft/blockstates/water.json", r#"{"variants":{"":{"model":"block/empty"}}}"#)
        .add("assets/minecraft/blockstates/chest.json", r#"{"variants":{"":{"model":"block/empty"}}}"#)
        .add(
            "assets/minecraft/blockstates/fence.json",
            r#"{"multipart":[{"apply":{"model":"block/post"}},{"when":{"north":"true"},"apply":{"model":"block/arm"}}]}"#,
        )
        .add("assets/minecraft/models/block/stone.json", cube(false, "block/stone"))
        .add("assets/minecraft/models/block/glass.json", cube(false, "block/glass"))
        .add("assets/minecraft/models/block/grass_block.json", cube(true, "block/grass_block"))
        .add("assets/minecraft/models/block/oak_leaves.json", cube(true, "block/leaf"))
        .add("assets/minecraft/models/block/slab_bottom.json", slab(0, 8))
        .add("assets/minecraft/models/block/slab_top.json", slab(8, 16))
        .add("assets/minecraft/models/block/empty.json", r#"{"textures":{"particle":"block/stone"}}"#)
        .add(
            "assets/minecraft/models/block/post.json",
            r##"{"textures":{"all":"block/stone"},"elements":[{"from":[6,0,6],"to":[10,16,10],"faces":{"north":{"texture":"#all"},"up":{"texture":"#all","cullface":"up"}}}]}"##,
        )
        .add(
            "assets/minecraft/models/block/arm.json",
            r##"{"textures":{"all":"block/stone"},"elements":[{"from":[7,6,0],"to":[9,9,6],"faces":{"up":{"texture":"#all"}}}]}"##,
        )
        .add("assets/minecraft/textures/block/stone.png", tex(120))
        .add("assets/minecraft/textures/block/glass.png", glass)
        .add("assets/minecraft/textures/block/grass_block.png", tex(200))
        .add("assets/minecraft/textures/block/leaf.png", leaf)
        .add("assets/minecraft/textures/colormap/grass.png", colormap(()))
        .add("assets/minecraft/textures/colormap/foliage.png", png(256, 256, |x, y| [x as u8, y as u8, 9, 255]))
        .add("data/minecraft/worldgen/biome/plains.json", biome(0.8, 0.4, ""))
        .add("data/minecraft/worldgen/biome/desert.json", biome(2.0, 0.0, ""))
        .add(
            "data/minecraft/worldgen/biome/swamp.json",
            biome(0.8, 0.9, r##","grass_color_modifier":"swamp","foliage_color":"#6a7039""##),
        )
        .add(
            "version.json",
            r#"{"id":"9.9","world_version":9000,"pack_version":{"resource":1}}"#,
        )
        .build();
    Assets::from_jar(jar).unwrap()
}

/// World block coordinates
pub type Pos = (i32, i32, i32);

/// A block as the tests write it: name and properties
pub type TestBlock = (String, Vec<(String, String)>);

/// A region's chunks as `region_bytes` takes them: local x, local z, compression id, payload
type RawChunks = Vec<(u32, u32, u8, Vec<u8>)>;

/// What a test world's chunk holds
#[derive(Clone, Debug, Default)]
pub struct ChunkSpec {
    pub cx: i32,
    pub cz: i32,
    /// World coordinates to (name, properties)
    pub blocks: BTreeMap<Pos, TestBlock>,
    /// The biome of the whole chunk (default `minecraft:plains`)
    pub biome: Option<String>,
    /// Biome of one 4 x 4 x 4 cell: (section y, cell index `cy*16 + cz*4 + cx`) to name
    pub biome_cells: BTreeMap<(i32, usize), String>,
    pub status: Option<String>,
    /// The 1.13 to 1.15 layout (`Level`, `Sections`, spanning longs)
    pub old_layout: bool,
    /// Extra sections to write even with nothing in them (so the chunk reaches them)
    pub also_sections: Vec<i32>,
}

impl ChunkSpec {
    pub fn new(cx: i32, cz: i32) -> Self {
        ChunkSpec {
            cx,
            cz,
            ..ChunkSpec::default()
        }
    }

    pub fn set(&mut self, x: i32, y: i32, z: i32, name: &str, props: &[(&str, &str)]) -> &mut Self {
        self.blocks.insert(
            (x, y, z),
            (
                name.to_string(),
                props
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ),
        );
        self
    }

    /// A filled box of one block (inclusive corners)
    pub fn fill(&mut self, a: (i32, i32, i32), b: (i32, i32, i32), name: &str) -> &mut Self {
        for y in a.1..=b.1 {
            for z in a.2..=b.2 {
                for x in a.0..=b.0 {
                    self.set(x, y, z, name, &[]);
                }
            }
        }
        self
    }

    pub fn nbt(&self) -> Tag {
        let mut secs: BTreeMap<i32, Vec<(Pos, &TestBlock)>> = BTreeMap::new();
        for (p, b) in &self.blocks {
            secs.entry(p.1.div_euclid(16)).or_default().push((*p, b));
        }
        for s in &self.also_sections {
            secs.entry(*s).or_default();
        }
        let biome = self
            .biome
            .clone()
            .unwrap_or_else(|| "minecraft:plains".into());
        let mut sections = Vec::new();
        for (sy, blocks) in &secs {
            // palette: air first, then in order of appearance
            let mut pal: Vec<(String, Vec<(String, String)>)> =
                vec![("minecraft:air".into(), vec![])];
            let mut idx = vec![0u16; 4096];
            for ((x, y, z), (n, p)) in blocks {
                let key = (n.clone(), p.clone());
                let i = pal.iter().position(|e| *e == key).unwrap_or_else(|| {
                    pal.push(key);
                    pal.len() - 1
                });
                let (lx, ly, lz) = (x.rem_euclid(16), y.rem_euclid(16), z.rem_euclid(16));
                idx[(ly * 256 + lz * 16 + lx) as usize] = i as u16;
            }
            let palette = Tag::List(
                pal.iter()
                    .map(|(n, p)| {
                        let props: Vec<(&str, &str)> =
                            p.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
                        entry(n, &props)
                    })
                    .collect(),
            );
            let bits = block_bits(pal.len());
            if self.old_layout {
                sections.push(compound(vec![
                    ("Y", Tag::Byte(*sy as i8)),
                    ("Palette", palette),
                    ("BlockStates", Tag::LongArray(pack_spanning(&idx, bits))),
                ]));
            } else {
                let mut bs = vec![("palette", palette)];
                if pal.len() > 1 {
                    bs.push(("data", Tag::LongArray(pack_padded(&idx, bits))));
                }
                // biomes: the chunk's, with this section's cells overridden
                let cells: Vec<(usize, &String)> = self
                    .biome_cells
                    .iter()
                    .filter(|((s, _), _)| s == sy)
                    .map(|((_, c), n)| (*c, n))
                    .collect();
                let mut bpal = vec![biome.clone()];
                let mut bidx = vec![0u16; 64];
                for (c, n) in cells {
                    let i = bpal.iter().position(|b| b == n).unwrap_or_else(|| {
                        bpal.push(n.clone());
                        bpal.len() - 1
                    });
                    bidx[c] = i as u16;
                }
                let mut b = vec![(
                    "palette",
                    Tag::List(bpal.iter().map(|s| string(s)).collect()),
                )];
                if bpal.len() > 1 {
                    let bits = (usize::BITS - (bpal.len() - 1).leading_zeros()) as usize;
                    b.push(("data", Tag::LongArray(pack_padded(&bidx, bits))));
                }
                sections.push(compound(vec![
                    ("Y", Tag::Byte(*sy as i8)),
                    ("block_states", compound(bs)),
                    ("biomes", compound(b)),
                ]));
            }
        }
        let status = self.status.clone().unwrap_or_else(|| {
            if self.old_layout {
                "postprocessed".into()
            } else {
                "minecraft:full".into()
            }
        });
        if self.old_layout {
            compound(vec![
                ("DataVersion", Tag::Int(1519)),
                (
                    "Level",
                    compound(vec![
                        ("xPos", Tag::Int(self.cx)),
                        ("zPos", Tag::Int(self.cz)),
                        ("Status", string(&status)),
                        ("Biomes", Tag::IntArray(vec![1; 256])),
                        ("Sections", Tag::List(sections)),
                    ]),
                ),
            ])
        } else {
            compound(vec![
                ("DataVersion", Tag::Int(9000)),
                ("xPos", Tag::Int(self.cx)),
                ("zPos", Tag::Int(self.cz)),
                ("Status", string(&status)),
                ("sections", Tag::List(sections)),
            ])
        }
    }
}

/// Writes a save with the given chunks into a fresh temp folder, `dimensions/minecraft/overworld`
/// layout unless `old` says the pre-1.16 `region` folder
pub fn write_world(
    tag: &str,
    version: &str,
    data_version: i32,
    chunks: &[ChunkSpec],
    old: bool,
) -> PathBuf {
    write_world_with(tag, version, data_version, chunks, &[], old)
}

/// The same, plus chunks given as the bytes of their NBT document (or of anything else, to be read
/// as damaged), by chunk coordinates
pub fn write_world_with(
    tag: &str,
    version: &str,
    data_version: i32,
    chunks: &[ChunkSpec],
    raw: &[(i32, i32, Vec<u8>)],
    old: bool,
) -> PathBuf {
    let dir = temp_dir(tag);
    let save = dir.join("save");
    std::fs::create_dir_all(&save).unwrap();
    let level = compound(vec![(
        "Data",
        compound(vec![
            ("DataVersion", Tag::Int(data_version)),
            ("Version", compound(vec![("Name", string(version))])),
            ("LevelName", string("T")),
            ("SpawnX", Tag::Int(8)),
            ("SpawnY", Tag::Int(70)),
            ("SpawnZ", Tag::Int(8)),
        ]),
    )]);
    std::fs::write(save.join("level.dat"), gzip(&document("", &level))).unwrap();
    let rdir = if old {
        save.join("region")
    } else {
        save.join("dimensions/minecraft/overworld/region")
    };
    std::fs::create_dir_all(&rdir).unwrap();
    let mut by_region: BTreeMap<(i32, i32), RawChunks> = BTreeMap::new();
    for c in chunks {
        let raw = document("", &c.nbt());
        by_region
            .entry((c.cx.div_euclid(32), c.cz.div_euclid(32)))
            .or_default()
            .push((
                c.cx.rem_euclid(32) as u32,
                c.cz.rem_euclid(32) as u32,
                2,
                zlib(&raw),
            ));
    }
    for (cx, cz, bytes) in raw {
        by_region
            .entry((cx.div_euclid(32), cz.div_euclid(32)))
            .or_default()
            .push((
                cx.rem_euclid(32) as u32,
                cz.rem_euclid(32) as u32,
                2,
                zlib(bytes),
            ));
    }
    for ((rx, rz), list) in by_region {
        std::fs::write(rdir.join(format!("r.{rx}.{rz}.mca")), region_bytes(&list)).unwrap();
    }
    save
}

pub fn cleanup(save: &Path) {
    if let Some(parent) = save.parent() {
        let _ = std::fs::remove_dir_all(parent);
    }
}
