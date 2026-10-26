//! The blocks of the chunks in a window, as state ids in compact sections, and the loader that
//! fills them from the region files.
//!
//! Memory is bounded three ways: the number of chunks in the window, the bytes the dense sections
//! take (8 KiB each; a section that is one state is a single number), and the number of distinct
//! states. A real chunk is a few tens of KiB here.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};

use super::biomes::{BiomeTable, NO_BIOME};
use super::blocks::{StateTable, AIR};
use super::chunk::{is_finished, parse_chunk, Chunk, ChunkError, Indices, SECTION_BLOCKS};
use super::nbt;
use super::region::Region;

#[derive(Clone, Debug)]
pub enum Sec {
    Uniform(u16),
    Dense(Box<[u16; SECTION_BLOCKS]>),
}

#[derive(Clone, Debug)]
pub enum BiomeSec {
    Uniform(u16),
    Cells(Box<[u16; 64]>),
}

#[derive(Clone, Debug)]
pub struct ChunkBlocks {
    /// The section coordinate of `secs[0]`
    pub min_sec: i32,
    pub secs: Vec<Sec>,
    pub biomes: Vec<BiomeSec>,
}

impl ChunkBlocks {
    pub fn bytes(&self) -> usize {
        self.secs
            .iter()
            .map(|s| match s {
                Sec::Uniform(_) => 2,
                Sec::Dense(_) => SECTION_BLOCKS * 2,
            })
            .sum::<usize>()
            + self
                .biomes
                .iter()
                .map(|s| match s {
                    BiomeSec::Uniform(_) => 2,
                    BiomeSec::Cells(_) => 128,
                })
                .sum::<usize>()
            + 64
    }
}

/// Chunks by coordinate over a rectangle of chunk coordinates
pub struct BlockGrid {
    x0: i32,
    z0: i32,
    w: i32,
    h: i32,
    chunks: Vec<Option<ChunkBlocks>>,
}

impl BlockGrid {
    /// Inclusive chunk coordinates
    pub fn new(x0: i32, z0: i32, x1: i32, z1: i32) -> BlockGrid {
        let w = (x1 as i64 - x0 as i64 + 1).clamp(0, 1 << 20) as i32;
        let h = (z1 as i64 - z0 as i64 + 1).clamp(0, 1 << 20) as i32;
        BlockGrid {
            x0,
            z0,
            w,
            h,
            chunks: (0..(w as usize * h as usize)).map(|_| None).collect(),
        }
    }

    fn slot(&self, cx: i32, cz: i32) -> Option<usize> {
        let (dx, dz) = (cx.checked_sub(self.x0)?, cz.checked_sub(self.z0)?);
        if dx < 0 || dz < 0 || dx >= self.w || dz >= self.h {
            return None;
        }
        Some(dz as usize * self.w as usize + dx as usize)
    }

    pub fn set(&mut self, cx: i32, cz: i32, c: ChunkBlocks) {
        if let Some(i) = self.slot(cx, cz) {
            self.chunks[i] = Some(c);
        }
    }

    pub fn chunk(&self, cx: i32, cz: i32) -> Option<&ChunkBlocks> {
        self.slot(cx, cz).and_then(|i| self.chunks[i].as_ref())
    }

    /// The state at a block position; [`AIR`] where there is no chunk or no section
    pub fn get(&self, x: i32, y: i32, z: i32) -> u16 {
        let Some(c) = self.chunk(x >> 4, z >> 4) else {
            return AIR;
        };
        let i = (y >> 4) as i64 - c.min_sec as i64;
        if i < 0 || i >= c.secs.len() as i64 {
            return AIR;
        }
        match &c.secs[i as usize] {
            Sec::Uniform(v) => *v,
            Sec::Dense(d) => d[(((y & 15) << 8) | ((z & 15) << 4) | (x & 15)) as usize],
        }
    }

    /// The biome id at a block position ([`NO_BIOME`] where unknown)
    pub fn biome(&self, x: i32, y: i32, z: i32) -> u16 {
        let Some(c) = self.chunk(x >> 4, z >> 4) else {
            return NO_BIOME;
        };
        let i = (y >> 4) as i64 - c.min_sec as i64;
        if i < 0 || i >= c.biomes.len() as i64 {
            return NO_BIOME;
        }
        match &c.biomes[i as usize] {
            BiomeSec::Uniform(v) => *v,
            BiomeSec::Cells(b) => {
                b[((((y & 15) >> 2) << 4) | (((z & 15) >> 2) << 2) | ((x & 15) >> 2)) as usize]
            }
        }
    }
}

/// Turns a parsed chunk's palettes into state ids
pub fn to_blocks(
    c: &Chunk,
    table: &mut StateTable,
    biomes: &mut BiomeTable,
) -> Result<ChunkBlocks> {
    let (Some(first), Some(last)) = (c.sections.first(), c.sections.last()) else {
        return Ok(ChunkBlocks {
            min_sec: 0,
            secs: Vec::new(),
            biomes: Vec::new(),
        });
    };
    let n = (last.y as i64 - first.y as i64 + 1) as usize;
    let mut secs: Vec<Sec> = (0..n).map(|_| Sec::Uniform(AIR)).collect();
    let mut bio: Vec<BiomeSec> = (0..n).map(|_| BiomeSec::Uniform(NO_BIOME)).collect();
    for s in &c.sections {
        let i = (s.y - first.y) as usize;
        let ids: Vec<u16> = s
            .palette
            .iter()
            .map(|p| table.resolve(&p.name, &p.props))
            .collect::<Result<_>>()?;
        secs[i] = match &s.blocks {
            Indices::Uniform => Sec::Uniform(ids[0]),
            Indices::Dense(d) => {
                let mut out = Box::new([AIR; SECTION_BLOCKS]);
                for (o, &p) in out.iter_mut().zip(d.iter()) {
                    *o = ids[p as usize];
                }
                Sec::Dense(out)
            }
        };
        if !s.biome_palette.is_empty() {
            let bids: Vec<u16> = s.biome_palette.iter().map(|b| biomes.resolve(b)).collect();
            bio[i] = match &s.biomes {
                None => BiomeSec::Uniform(bids[0]),
                Some(cells) => {
                    let mut out = Box::new([NO_BIOME; 64]);
                    for (o, &p) in out.iter_mut().zip(cells.iter()) {
                        *o = bids[p as usize];
                    }
                    BiomeSec::Cells(out)
                }
            };
        }
    }
    Ok(ChunkBlocks {
        min_sec: first.y,
        secs,
        biomes: bio,
    })
}

/// What loading the chunks found
#[derive(Clone, Debug, Default)]
pub struct LoadStats {
    /// Chunks the region tables list inside the window
    pub listed: usize,
    /// Chunks of the window that were read into the grid
    pub loaded: usize,
    /// Chunks of the one-chunk border around the window that were read (they only serve culling)
    pub apron_loaded: usize,
    /// Window chunks not finished by the game, by status (left out unless `include_unfinished`)
    pub unfinished: BTreeMap<String, usize>,
    pub unfinished_included: bool,
    /// Window chunks with numeric block ids (before 1.13), and the data versions they had
    pub legacy: usize,
    pub legacy_versions: Vec<Option<i64>>,
    /// Window chunks that failed, with why (the first few), and how many
    pub failed: Vec<(i32, i32, String)>,
    pub failed_count: usize,
    pub apron_failed: usize,
    /// Window chunks that store biomes as numbers
    pub numeric_biomes: usize,
    pub block_bytes: usize,
}

pub struct LoadLimits {
    pub max_chunks: usize,
    pub max_block_bytes: usize,
    /// Read chunks the game had not finished generating (their blocks are what the save holds, not
    /// what the game would show)
    pub include_unfinished: bool,
}

/// Region coordinates that hold chunk coordinates `lo..=hi`
fn region_span(lo: i32, hi: i32) -> std::ops::RangeInclusive<i32> {
    (lo >> 5)..=(hi >> 5)
}

/// Whether a region file holds a chunk the game had finished and this crate can read: the first one
/// found ends the search. Used to list only regions that would show something.
pub fn has_finished_chunk(path: &Path) -> bool {
    let Ok(mut r) = Region::open(path) else {
        return false;
    };
    for lz in 0..32u32 {
        for lx in 0..32u32 {
            if !r.has_chunk(lx, lz) {
                continue;
            }
            let Ok(Some(raw)) = r.read_chunk(lx, lz) else {
                continue;
            };
            let Ok((_, root)) = nbt::parse(&raw) else {
                continue;
            };
            let (cx, cz) = (r.x * 32 + lx as i32, r.z * 32 + lz as i32);
            if let Ok(c) = parse_chunk(&root, Some((cx, cz))) {
                if is_finished(&c.status) {
                    return true;
                }
            }
        }
    }
    false
}

/// Reads the chunks `window` (inclusive chunk coordinates) and a one-chunk border around it from
/// the region files in `region_dir`
pub fn load(
    region_dir: &Path,
    window: (i32, i32, i32, i32),
    table: &mut StateTable,
    biomes: &mut BiomeTable,
    limits: &LoadLimits,
) -> Result<(BlockGrid, LoadStats)> {
    let (wx0, wz0, wx1, wz1) = window;
    if (wx1 as i64 - wx0 as i64 + 3) * (wz1 as i64 - wz0 as i64 + 3) > 1 << 22 {
        bail!("the window is wider than 4 million chunks");
    }
    let (ax0, az0, ax1, az1) = (
        wx0.saturating_sub(1),
        wz0.saturating_sub(1),
        wx1.saturating_add(1),
        wz1.saturating_add(1),
    );
    let in_window = |cx: i32, cz: i32| cx >= wx0 && cx <= wx1 && cz >= wz0 && cz <= wz1;
    let mut stats = LoadStats {
        unfinished_included: limits.include_unfinished,
        ..LoadStats::default()
    };
    // the regions that exist, and a count of the chunks they list in the window, before any chunk
    // is read. Files are opened one at a time: a radius of 8 is 361 of them, more than a process
    // is usually allowed to hold open.
    let mut regions: Vec<(i32, i32)> = Vec::new();
    for rz in region_span(az0, az1) {
        for rx in region_span(ax0, ax1) {
            let p = region_dir.join(format!("r.{rx}.{rz}.mca"));
            if !p.is_file() {
                continue;
            }
            let r = Region::open(&p)?;
            for lz in 0..32u32 {
                for lx in 0..32u32 {
                    let (cx, cz) = (rx * 32 + lx as i32, rz * 32 + lz as i32);
                    if in_window(cx, cz) && r.has_chunk(lx, lz) {
                        stats.listed += 1;
                    }
                }
            }
            regions.push((rx, rz));
        }
    }
    if stats.listed > limits.max_chunks {
        bail!(
            "the window lists {} chunks; the limit is {} (use a smaller window: --chunks, or a smaller --radius)",
            stats.listed,
            limits.max_chunks
        );
    }
    let mut grid = BlockGrid::new(ax0, az0, ax1, az1);
    for (rx, rz) in regions {
        let mut r = Region::open(&region_dir.join(format!("r.{rx}.{rz}.mca")))?;
        for lz in 0..32u32 {
            for lx in 0..32u32 {
                let (cx, cz) = (r.x * 32 + lx as i32, r.z * 32 + lz as i32);
                if cx < ax0 || cx > ax1 || cz < az0 || cz > az1 || !r.has_chunk(lx, lz) {
                    continue;
                }
                let inw = in_window(cx, cz);
                let res = (|| -> std::result::Result<Option<ChunkBlocks>, ChunkError> {
                    let Some(raw) = r.read_chunk(lx, lz)? else {
                        return Ok(None);
                    };
                    let (_, root) = nbt::parse(&raw).context("NBT")?;
                    let ch = parse_chunk(&root, Some((cx, cz)))?;
                    if !is_finished(&ch.status) {
                        if inw {
                            *stats
                                .unfinished
                                .entry(ch.status.clone().unwrap_or_default())
                                .or_default() += 1;
                        }
                        if !limits.include_unfinished {
                            return Ok(None);
                        }
                    }
                    if inw && ch.numeric_biomes {
                        stats.numeric_biomes += 1;
                    }
                    Ok(Some(to_blocks(&ch, table, biomes)?))
                })();
                match res {
                    Ok(Some(b)) => {
                        stats.block_bytes += b.bytes();
                        if stats.block_bytes > limits.max_block_bytes {
                            bail!(
                                "the blocks of the window need more than {} MiB; use a smaller window",
                                limits.max_block_bytes >> 20
                            );
                        }
                        if inw {
                            stats.loaded += 1;
                        } else {
                            stats.apron_loaded += 1;
                        }
                        grid.set(cx, cz, b);
                    }
                    Ok(None) => {}
                    Err(ChunkError::Legacy { data_version }) => {
                        if inw {
                            stats.legacy += 1;
                            if !stats.legacy_versions.contains(&data_version) {
                                stats.legacy_versions.push(data_version);
                            }
                        }
                    }
                    Err(ChunkError::Bad(e)) => {
                        if inw {
                            stats.failed_count += 1;
                            if stats.failed.len() < 20 {
                                stats.failed.push((cx, cz, format!("{e:#}")));
                            }
                        } else {
                            stats.apron_failed += 1;
                        }
                    }
                }
            }
        }
    }
    Ok((grid, stats))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_lookups() {
        let mut g = BlockGrid::new(-1, -1, 0, 0);
        let mut dense = Box::new([AIR; SECTION_BLOCKS]);
        // block (3, 5, 7) of the section: y*256 + z*16 + x
        dense[5 * 256 + 7 * 16 + 3] = 9;
        g.set(
            -1,
            0,
            ChunkBlocks {
                min_sec: -2,
                secs: vec![Sec::Uniform(4), Sec::Dense(dense)],
                biomes: vec![BiomeSec::Uniform(2), BiomeSec::Uniform(3)],
            },
        );
        // chunk -1,0 covers x -16..-1, z 0..15; section -2 is y -32..-17, section -1 is y -16..-1
        assert_eq!(g.get(-5, -30, 3), 4);
        assert_eq!(g.get(-16 + 3, -16 + 5, 7), 9);
        assert_eq!(g.get(-16 + 4, -16 + 5, 7), AIR);
        // outside the sections, outside the chunk, outside the grid
        assert_eq!(g.get(-5, 0, 3), AIR);
        assert_eq!(g.get(-5, -40, 3), AIR);
        assert_eq!(g.get(5, 0, 3), AIR);
        assert_eq!(g.get(i32::MAX, 0, i32::MIN), AIR);
        assert_eq!(g.biome(-5, -20, 3), 2);
        assert_eq!(g.biome(-5, -1, 3), 3);
        assert_eq!(g.biome(0, 0, 0), NO_BIOME);
    }

    #[test]
    fn biome_cells_are_four_blocks_wide() {
        let mut cells = Box::new([0u16; 64]);
        // cell (cx 1, cy 2, cz 3) = index cy*16 + cz*4 + cx
        cells[2 * 16 + 3 * 4 + 1] = 7;
        let mut g = BlockGrid::new(0, 0, 0, 0);
        g.set(
            0,
            0,
            ChunkBlocks {
                min_sec: 0,
                secs: vec![Sec::Uniform(AIR)],
                biomes: vec![BiomeSec::Cells(cells)],
            },
        );
        assert_eq!(g.biome(4, 8, 12), 7);
        assert_eq!(g.biome(7, 11, 15), 7);
        assert_eq!(g.biome(8, 8, 12), 0);
        assert_eq!(g.biome(4, 12, 12), 0);
    }

    #[test]
    fn region_spans() {
        assert_eq!(region_span(-1, 0), -1..=0);
        assert_eq!(region_span(31, 32), 0..=1);
        assert_eq!(region_span(-33, -32), -2..=-1);
        assert_eq!(region_span(-64, -33), -2..=-2);
    }
}
