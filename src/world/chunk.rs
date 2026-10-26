//! A chunk's NBT as the game stores it, down to block-state indices per 16 x 16 x 16 section.
//!
//! The formats this reads (all of them are palettes of block states by name, so the names are the
//! jar's own blockstate files):
//!
//! | versions | where the sections are | blocks | biomes |
//! |----------|------------------------|--------|--------|
//! | 1.13 to 1.17 | `Level.Sections[]` | `Palette` and `BlockStates` (long array) | `Level.Biomes`, numeric ids: not read |
//! | 1.18 and later | `sections[]` | `block_states.palette` and `.data` | `biomes.palette` (names) and `.data`, one entry per 4 x 4 x 4 |
//!
//! Section `Y` is a signed byte, so a world that goes below 0 (1.18: -64) has negative sections. The
//! long arrays are packed with `max(4, ceil(log2(palette size)))` bits per block, block order
//! `y * 256 + z * 16 + x`. **1.16 changed the packing**: before it an entry can start in one long and
//! end in the next; since it, every long holds `64 / bits` whole entries and the rest of its bits are
//! unused. A save can hold chunks of both (each chunk keeps the format of the version that saved it).
//! The two are told apart by the array's length, which differs for every bit width that is not 4, 8
//! or 16 (where the two are the same bytes), so the chunk's `DataVersion` does not have to be right.
//!
//! **Before 1.13 (the flattening)** a chunk stores numeric block ids and 4-bit data values
//! (`Blocks`, `Add`, `Data`). Which block and which state a pair means is in the game's code (the id
//! table, and each block's `getStateFromMeta`; the derived properties a model needs, a fence's arms or
//! a stair's shape, are computed at run time), so those chunks are reported as [`ChunkError::Legacy`]
//! and not read.
//!
//! The 26.x writer, as found in the saves of this machine, writes a palette entry as a plain string
//! (a block with no properties, when every entry of the list is one), as `{"": "minecraft:name"}`
//! (the same, beside entries that have properties), or as `{id, properties}`; 1.13 to 1.21 write
//! `{Name, Properties}`. All four are read.

use std::collections::BTreeMap;

use anyhow::{anyhow, bail, Context};

use super::nbt::Tag;

pub const SECTION_BLOCKS: usize = 4096;

/// More than a section can hold (4096 blocks): a palette this long is not a real one
pub const MAX_PALETTE: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PaletteEntry {
    /// `minecraft:oak_stairs`
    pub name: String,
    /// Sorted by property name
    pub props: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Indices {
    /// Every block is palette entry 0
    Uniform,
    /// Palette index per block, `y * 256 + z * 16 + x`
    Dense(Box<[u16; SECTION_BLOCKS]>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    /// Signed section coordinate; blocks are at `y * 16` to `y * 16 + 15`
    pub y: i32,
    pub palette: Vec<PaletteEntry>,
    pub blocks: Indices,
    /// Biome names of the palette (1.18 and later)
    pub biome_palette: Vec<String>,
    /// Palette index per 4 x 4 x 4 cell, `cy * 16 + cz * 4 + cx`; `None`: all entry 0
    pub biomes: Option<Box<[u8; 64]>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chunk {
    pub x: i32,
    pub z: i32,
    pub data_version: Option<i64>,
    pub status: Option<String>,
    pub sections: Vec<Section>,
    /// The chunk stores its biomes as numeric ids (before 1.18), which name a biome only through
    /// the game's code
    pub numeric_biomes: bool,
}

#[derive(Debug)]
pub enum ChunkError {
    /// Numeric block ids and data values (before 1.13)
    Legacy {
        data_version: Option<i64>,
    },
    Bad(anyhow::Error),
}

impl std::fmt::Display for ChunkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChunkError::Legacy { data_version } => write!(
                f,
                "numeric block ids and data values (before 1.13, DataVersion {}): the id table and each block's data-value meaning are in the game's code",
                data_version.map(|v| v.to_string()).unwrap_or_else(|| "none".into())
            ),
            ChunkError::Bad(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for ChunkError {}

impl From<anyhow::Error> for ChunkError {
    fn from(e: anyhow::Error) -> Self {
        ChunkError::Bad(e)
    }
}

fn ceil_log2(n: usize) -> usize {
    if n <= 1 {
        0
    } else {
        (usize::BITS - (n - 1).leading_zeros()) as usize
    }
}

/// Bits per block for a palette of `n` entries
pub fn block_bits(n: usize) -> usize {
    ceil_log2(n).max(4)
}

/// Unpacks `count` entries of `bits` bits. `data` is in the packing of 1.13 to 1.15 (entries span
/// longs) or of 1.16 and later (whole entries per long); the length says which.
pub fn unpack(data: &[i64], bits: usize, count: usize) -> anyhow::Result<Vec<u16>> {
    if bits == 0 || bits > 16 {
        bail!("{bits} bits per entry");
    }
    let mask = (1u64 << bits) - 1;
    let spanning_len = (count * bits).div_ceil(64);
    let per_long = 64 / bits;
    let padded_len = count.div_ceil(per_long);
    let mut out = Vec::with_capacity(count);
    if data.len() == padded_len {
        for i in 0..count {
            let l = i / per_long;
            let off = (i % per_long) * bits;
            out.push((((data[l] as u64) >> off) & mask) as u16);
        }
    } else if data.len() == spanning_len {
        for i in 0..count {
            let bit = i * bits;
            let (l, off) = (bit >> 6, bit & 63);
            let mut v = (data[l] as u64) >> off;
            if off + bits > 64 {
                v |= (data[l + 1] as u64) << (64 - off);
            }
            out.push((v & mask) as u16);
        }
    } else {
        bail!(
            "a packed array of {} longs for {count} entries of {bits} bits: expected {padded_len} (1.16 and later) or {spanning_len} (1.13 to 1.15)",
            data.len()
        );
    }
    Ok(out)
}

fn palette_entry(t: &Tag) -> anyhow::Result<PaletteEntry> {
    let (name, raw_props) = match t {
        Tag::String(s) => (s.as_str(), None),
        Tag::Compound(_) => {
            let name = t
                .get("Name")
                .or_else(|| t.get("id"))
                .or_else(|| t.get(""))
                .and_then(Tag::as_str)
                .context("a palette entry with no name")?;
            (name, t.get("Properties").or_else(|| t.get("properties")))
        }
        other => bail!("a palette entry of NBT type {}", other.type_id()),
    };
    if name.is_empty() {
        bail!("a palette entry with an empty name");
    }
    let mut props = Vec::new();
    if let Some(p) = raw_props {
        let c = p.as_compound().context("`Properties` is not a compound")?;
        let mut m = BTreeMap::new();
        for (k, v) in c {
            let v = v
                .as_str()
                .with_context(|| format!("property {k} of {name} is not a string"))?;
            m.insert(k.clone(), v.to_string());
        }
        props = m.into_iter().collect();
    }
    Ok(PaletteEntry {
        name: name.to_string(),
        props,
    })
}

fn palette(t: &Tag) -> anyhow::Result<Vec<PaletteEntry>> {
    let l = t.as_list().context("palette is not a list")?;
    if l.is_empty() || l.len() > MAX_PALETTE {
        bail!("a palette of {} entries", l.len());
    }
    l.iter().map(palette_entry).collect()
}

fn read_blocks(pal: &Tag, data: Option<&Tag>) -> anyhow::Result<(Vec<PaletteEntry>, Indices)> {
    let palette = palette(pal)?;
    if palette.len() == 1 {
        return Ok((palette, Indices::Uniform));
    }
    let data = data
        .context("a palette of several entries and no block data")?
        .as_long_array()
        .context("block data is not a long array")?;
    let idx = unpack(data, block_bits(palette.len()), SECTION_BLOCKS)?;
    let mut dense = Box::new([0u16; SECTION_BLOCKS]);
    for (d, i) in dense.iter_mut().zip(idx) {
        if i as usize >= palette.len() {
            bail!("block index {i} in a palette of {}", palette.len());
        }
        *d = i;
    }
    Ok((palette, Indices::Dense(dense)))
}

/// Biome cells: one palette index per 4 x 4 x 4 cell of a section
type BiomeCells = Box<[u8; 64]>;

fn read_biomes(b: &Tag) -> anyhow::Result<(Vec<String>, Option<BiomeCells>)> {
    let pal = b
        .get("palette")
        .and_then(Tag::as_list)
        .context("biomes without a palette")?;
    if pal.is_empty() || pal.len() > 64 {
        bail!("a biome palette of {} entries", pal.len());
    }
    let names: Vec<String> = pal
        .iter()
        .map(|t| {
            t.as_str()
                .map(String::from)
                .context("a biome name that is not a string")
        })
        .collect::<anyhow::Result<_>>()?;
    if names.len() == 1 {
        return Ok((names, None));
    }
    let data = b
        .get("data")
        .context("a biome palette of several entries and no data")?
        .as_long_array()
        .context("biome data is not a long array")?;
    let bits = ceil_log2(names.len());
    // biomes only exist in the 1.18 packing, which pads
    let per_long = 64 / bits;
    if data.len() != 64usize.div_ceil(per_long) {
        bail!(
            "biome data of {} longs for 64 entries of {bits} bits",
            data.len()
        );
    }
    let idx = unpack(data, bits, 64)?;
    let mut cells = Box::new([0u8; 64]);
    for (c, i) in cells.iter_mut().zip(idx) {
        if i as usize >= names.len() {
            bail!("biome index {i} in a palette of {}", names.len());
        }
        *c = i as u8;
    }
    Ok((names, Some(cells)))
}

/// Reads a chunk's root compound. `expect` is the chunk's place in its region: a chunk that says it
/// is somewhere else is refused (the game does the same).
pub fn parse_chunk(root: &Tag, expect: Option<(i32, i32)>) -> Result<Chunk, ChunkError> {
    if root.as_compound().is_none() {
        return Err(anyhow!("the chunk root is not a compound").into());
    }
    let (level, old) = match root.get("Level") {
        Some(l) if l.as_compound().is_some() => (l, true),
        _ => (root, false),
    };
    let data_version = root.get("DataVersion").and_then(Tag::as_i64);
    let coord = |k: &str| {
        level
            .get(k)
            .and_then(Tag::as_i64)
            .and_then(|v| i32::try_from(v).ok())
    };
    let (x, z) = match (coord("xPos"), coord("zPos")) {
        (Some(x), Some(z)) => (x, z),
        _ => return Err(anyhow!("the chunk has no xPos and zPos").into()),
    };
    if let Some((ex, ez)) = expect {
        if (x, z) != (ex, ez) {
            return Err(anyhow!("the chunk says it is at {x},{z} but sits at {ex},{ez}").into());
        }
    }
    let status = level.get("Status").and_then(Tag::as_str).map(String::from);
    let secs = match level.get(if old { "Sections" } else { "sections" }) {
        Some(s) => s.as_list().context("sections is not a list")?,
        None => &[],
    };
    // numbered ids first: say so before any other complaint
    if secs.iter().any(|s| s.get("Blocks").is_some()) {
        return Err(ChunkError::Legacy { data_version });
    }
    let mut sections = Vec::with_capacity(secs.len());
    for s in secs {
        let y = s
            .get("Y")
            .and_then(Tag::as_i64)
            .context("a section with no Y")?;
        let y = i32::try_from(y).context("section Y out of range")?;
        let (pal, data) = if old {
            (s.get("Palette"), s.get("BlockStates"))
        } else {
            let bs = s.get("block_states");
            (
                bs.and_then(|b| b.get("palette")),
                bs.and_then(|b| b.get("data")),
            )
        };
        // a section with no block data is a lighting-only one
        let Some(pal) = pal else { continue };
        let (palette, blocks) =
            read_blocks(pal, data).with_context(|| format!("section {y} of chunk {x},{z}"))?;
        let (biome_palette, biomes) = match s.get("biomes") {
            Some(b) if !old => {
                read_biomes(b).with_context(|| format!("biomes of section {y} of chunk {x},{z}"))?
            }
            _ => (Vec::new(), None),
        };
        sections.push(Section {
            y,
            palette,
            blocks,
            biome_palette,
            biomes,
        });
    }
    // sections in order, each once
    sections.sort_by_key(|s| s.y);
    if sections.windows(2).any(|w| w[0].y == w[1].y) {
        return Err(anyhow!("chunk {x},{z} lists a section twice").into());
    }
    Ok(Chunk {
        x,
        z,
        data_version,
        status,
        sections,
        numeric_biomes: old && level.get("Biomes").is_some(),
    })
}

/// Whether the chunk is a finished one. The statuses a save writes while a chunk is still being
/// generated (`minecraft:structure_starts`, `noise`, `features` ... ) mean its blocks are not final;
/// the game regenerates such a chunk when it is next needed.
pub fn is_finished(status: &Option<String>) -> bool {
    match status.as_deref() {
        None => true,
        Some(s) => matches!(
            s.strip_prefix("minecraft:").unwrap_or(s),
            // 1.14 and later; 1.13 writes `postprocessed` (and `fullchunk` for a chunk just made a
            // full one)
            "full" | "fullchunk" | "postprocessed"
        ),
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::world::nbt::write::*;

    pub fn pack_padded(idx: &[u16], bits: usize) -> Vec<i64> {
        let per = 64 / bits;
        let mut out = vec![0i64; idx.len().div_ceil(per)];
        for (i, &v) in idx.iter().enumerate() {
            out[i / per] |= (v as i64) << ((i % per) * bits);
        }
        out
    }

    pub fn pack_spanning(idx: &[u16], bits: usize) -> Vec<i64> {
        let mut out = vec![0u64; (idx.len() * bits).div_ceil(64)];
        for (i, &v) in idx.iter().enumerate() {
            let bit = i * bits;
            let (l, off) = (bit >> 6, bit & 63);
            out[l] |= (v as u64) << off;
            if off + bits > 64 {
                out[l + 1] |= (v as u64) >> (64 - off);
            }
        }
        out.into_iter().map(|v| v as i64).collect()
    }

    pub fn entry(name: &str, props: &[(&str, &str)]) -> Tag {
        let mut v = vec![("Name", string(name))];
        if !props.is_empty() {
            v.push((
                "Properties",
                compound(props.iter().map(|(k, x)| (*k, string(x))).collect()),
            ));
        }
        compound(v)
    }

    #[test]
    fn bits() {
        assert_eq!(block_bits(1), 4);
        assert_eq!(block_bits(16), 4);
        assert_eq!(block_bits(17), 5);
        assert_eq!(block_bits(43), 6);
        assert_eq!(block_bits(4096), 12);
        assert_eq!(ceil_log2(2), 1);
        assert_eq!(ceil_log2(3), 2);
        assert_eq!(ceil_log2(64), 6);
    }

    #[test]
    fn both_packings_decode_for_every_width() {
        for bits in 4..=12usize {
            let idx: Vec<u16> = (0..4096u32)
                .map(|i| ((i * 7 + 3) % (1 << bits)) as u16)
                .collect();
            assert_eq!(
                unpack(&pack_padded(&idx, bits), bits, 4096).unwrap(),
                idx,
                "padded {bits}"
            );
            assert_eq!(
                unpack(&pack_spanning(&idx, bits), bits, 4096).unwrap(),
                idx,
                "spanning {bits}"
            );
        }
        // a wrong length is an error
        assert!(unpack(&[0; 5], 4, 4096).is_err());
        assert!(unpack(&[0; 410], 6, 4096).is_ok());
        assert!(unpack(&[0; 411], 6, 4096).is_err());
        assert!(unpack(&[], 0, 4096).is_err());
        // the two packings really differ at 6 bits
        assert_ne!(
            pack_padded(&[1; 4096], 6).len(),
            pack_spanning(&[1; 4096], 6).len()
        );
    }

    fn modern_chunk(extra_section: Option<Tag>) -> Tag {
        let idx: Vec<u16> = (0..4096).map(|i| (i % 3) as u16).collect();
        let mut sections = vec![
            compound(vec![
                ("Y", Tag::Byte(-4)),
                (
                    "block_states",
                    compound(vec![("palette", Tag::List(vec![string("minecraft:air")]))]),
                ),
                (
                    "biomes",
                    compound(vec![(
                        "palette",
                        Tag::List(vec![string("minecraft:plains")]),
                    )]),
                ),
            ]),
            compound(vec![
                ("Y", Tag::Byte(0)),
                (
                    "block_states",
                    compound(vec![
                        (
                            "palette",
                            Tag::List(vec![
                                compound(vec![("", string("minecraft:air"))]),
                                compound(vec![
                                    ("id", string("minecraft:oak_log")),
                                    ("properties", compound(vec![("axis", string("x"))])),
                                ]),
                                entry("minecraft:stone", &[]),
                            ]),
                        ),
                        ("data", Tag::LongArray(pack_padded(&idx, 4))),
                    ]),
                ),
                (
                    "biomes",
                    compound(vec![
                        (
                            "palette",
                            Tag::List(vec![string("minecraft:plains"), string("minecraft:desert")]),
                        ),
                        ("data", Tag::LongArray(vec![0b1000])),
                    ]),
                ),
            ]),
            // lighting only
            compound(vec![
                ("Y", Tag::Byte(20)),
                ("SkyLight", Tag::ByteArray(vec![0; 2048])),
            ]),
        ];
        if let Some(e) = extra_section {
            sections.push(e);
        }
        compound(vec![
            ("DataVersion", Tag::Int(5023)),
            ("xPos", Tag::Int(33)),
            ("zPos", Tag::Int(-2)),
            ("Status", string("minecraft:full")),
            ("sections", Tag::List(sections)),
        ])
    }

    #[test]
    fn reads_a_modern_chunk() {
        let c = parse_chunk(&modern_chunk(None), Some((33, -2))).unwrap();
        assert_eq!((c.x, c.z), (33, -2));
        assert_eq!(c.sections.len(), 2, "the light-only section is skipped");
        assert_eq!(c.sections[0].y, -4);
        assert_eq!(c.sections[0].blocks, Indices::Uniform);
        let s = &c.sections[1];
        assert_eq!(s.palette[0].name, "minecraft:air");
        assert_eq!(
            s.palette[1],
            PaletteEntry {
                name: "minecraft:oak_log".into(),
                props: vec![("axis".into(), "x".into())]
            }
        );
        match &s.blocks {
            Indices::Dense(d) => {
                assert_eq!(d[0], 0);
                assert_eq!(d[1], 1);
                assert_eq!(d[2], 2);
                assert_eq!(d[3], 0);
            }
            _ => panic!(),
        }
        // biome cell 3 is entry 1 (bit 3 of the single long, 1 bit per entry)
        let b = s.biomes.as_ref().unwrap();
        assert_eq!(b[3], 1);
        assert_eq!(b[0], 0);
        assert_eq!(
            s.biome_palette,
            vec!["minecraft:plains", "minecraft:desert"]
        );
        assert!(is_finished(&c.status));
        assert!(!c.numeric_biomes);
    }

    #[test]
    fn a_chunk_in_the_wrong_place_or_unfinished_or_damaged() {
        assert!(parse_chunk(&modern_chunk(None), Some((0, 0))).is_err());
        assert!(!is_finished(&Some("minecraft:structure_starts".into())));
        assert!(is_finished(&Some("postprocessed".into())));
        assert!(is_finished(&Some("minecraft:full".into())));
        // an index outside the palette
        let mut idx = vec![0u16; 4096];
        idx[10] = 9;
        let bad = compound(vec![
            ("Y", Tag::Byte(1)),
            (
                "block_states",
                compound(vec![
                    (
                        "palette",
                        Tag::List(vec![string("minecraft:air"), string("minecraft:stone")]),
                    ),
                    ("data", Tag::LongArray(pack_padded(&idx, 4))),
                ]),
            ),
        ]);
        let e = parse_chunk(&modern_chunk(Some(bad)), None)
            .unwrap_err()
            .to_string();
        assert!(e.contains("block index 9"), "{e}");
        // two sections with the same Y
        let dup = compound(vec![
            ("Y", Tag::Byte(0)),
            (
                "block_states",
                compound(vec![("palette", Tag::List(vec![string("minecraft:air")]))]),
            ),
        ]);
        let e = parse_chunk(&modern_chunk(Some(dup)), None)
            .unwrap_err()
            .to_string();
        assert!(e.contains("twice"), "{e}");
        // an oversized palette
        let big = compound(vec![
            ("Y", Tag::Byte(5)),
            (
                "block_states",
                compound(vec![(
                    "palette",
                    Tag::List(
                        (0..5000)
                            .map(|i| string(&format!("minecraft:b{i}")))
                            .collect(),
                    ),
                )]),
            ),
        ]);
        assert!(parse_chunk(&modern_chunk(Some(big)), None).is_err());
        // not a compound
        assert!(parse_chunk(&Tag::Int(1), None).is_err());
        // a property that is not a string
        let e = palette_entry(&compound(vec![
            ("Name", string("minecraft:x")),
            ("Properties", compound(vec![("a", Tag::Byte(1))])),
        ]))
        .unwrap_err();
        assert!(e.to_string().contains("not a string"));
    }

    #[test]
    fn reads_the_1_13_layout_with_spanning_longs() {
        let idx: Vec<u16> = (0..4096).map(|i| (i % 5) as u16).collect();
        let sec = compound(vec![
            ("Y", Tag::Byte(3)),
            (
                "Palette",
                Tag::List(vec![
                    entry("minecraft:air", &[]),
                    entry("minecraft:stone", &[]),
                    entry(
                        "minecraft:oak_stairs",
                        &[("facing", "east"), ("half", "bottom")],
                    ),
                    entry("minecraft:dirt", &[]),
                    entry("minecraft:grass_block", &[("snowy", "false")]),
                ]),
            ),
            ("BlockStates", Tag::LongArray(pack_spanning(&idx, 4))),
        ]);
        let root = compound(vec![
            ("DataVersion", Tag::Int(1519)),
            (
                "Level",
                compound(vec![
                    ("xPos", Tag::Int(1)),
                    ("zPos", Tag::Int(2)),
                    ("Status", string("postprocessed")),
                    ("Biomes", Tag::IntArray(vec![1; 256])),
                    ("Sections", Tag::List(vec![sec])),
                ]),
            ),
        ]);
        let c = parse_chunk(&root, Some((1, 2))).unwrap();
        assert!(c.numeric_biomes);
        assert_eq!(c.sections[0].y, 3);
        assert_eq!(c.sections[0].palette[2].props.len(), 2);
        match &c.sections[0].blocks {
            Indices::Dense(d) => assert_eq!(&d[..6], &[0, 1, 2, 3, 4, 0]),
            _ => panic!(),
        }
        assert!(is_finished(&c.status));
    }

    #[test]
    fn numeric_ids_are_reported_as_such() {
        let root = compound(vec![(
            "Level",
            compound(vec![
                ("xPos", Tag::Int(0)),
                ("zPos", Tag::Int(0)),
                (
                    "Sections",
                    Tag::List(vec![compound(vec![
                        ("Y", Tag::Byte(0)),
                        ("Blocks", Tag::ByteArray(vec![0; 4096])),
                        ("Data", Tag::ByteArray(vec![0; 2048])),
                    ])]),
                ),
            ]),
        )]);
        match parse_chunk(&root, None) {
            Err(ChunkError::Legacy { data_version: None }) => {}
            other => panic!("{other:?}"),
        }
    }
}
