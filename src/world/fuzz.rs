//! Mutation fuzzing of everything a world file feeds: the tests mutate real-format bytes (bit
//! flips, truncations, lengths set to extreme values, spliced copies) and require that nothing
//! panics, hangs or allocates without bound; errors are the expected outcome. The number of rounds
//! is `MC_UNPACK_FUZZ` (default 400 per target); a failing round prints its seed.

use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};

use super::blocks::StateTable;
use super::chunk::{parse_chunk, ChunkError};
use super::compress::{decompress, tests as ctests, Compression};
use super::grid::to_blocks;
use super::mesher::{build, BuildOptions, Window};
use super::nbt::{self, write::document};
use super::region::{tests::region_bytes, tests::temp_dir, Region};
use super::save::World;
use super::testworld::*;

pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

const INTERESTING: [u32; 10] = [
    0,
    1,
    2,
    255,
    256,
    4095,
    4096,
    0x7FFF_FFFF,
    0x8000_0000,
    0xFFFF_FFFF,
];

pub fn mutate(src: &[u8], r: &mut Rng) -> Vec<u8> {
    let mut b = src.to_vec();
    for _ in 0..1 + r.below(4) {
        if b.is_empty() {
            b.push(r.next() as u8);
            continue;
        }
        match r.below(8) {
            0 => {
                let i = r.below(b.len());
                b[i] ^= 1 << r.below(8);
            }
            1 => {
                let i = r.below(b.len());
                b[i] = r.next() as u8;
            }
            2 => {
                let n = r.below(b.len() + 1);
                b.truncate(n);
            }
            3 => {
                for _ in 0..1 + r.below(16) {
                    b.push(r.next() as u8);
                }
            }
            4 => {
                // a 4-byte big-endian number set to something extreme
                if b.len() >= 4 {
                    let i = r.below(b.len() - 3);
                    let v = INTERESTING[r.below(INTERESTING.len())].to_be_bytes();
                    b[i..i + 4].copy_from_slice(&v);
                }
            }
            5 => {
                // a 2-byte length
                if b.len() >= 2 {
                    let i = r.below(b.len() - 1);
                    let v = (INTERESTING[r.below(INTERESTING.len())] as u16).to_be_bytes();
                    b[i..i + 2].copy_from_slice(&v);
                }
            }
            6 => {
                // splice a copy of one stretch over another
                let n = 1 + r.below(b.len().min(64));
                let from = r.below(b.len() - n + 1);
                let to = r.below(b.len() - n + 1);
                let piece = b[from..from + n].to_vec();
                b[to..to + n].copy_from_slice(&piece);
            }
            _ => {
                let i = r.below(b.len());
                let n = 1 + r.below(8);
                let end = (i + n).min(b.len());
                b.drain(i..end);
            }
        }
    }
    b
}

pub fn rounds(default: usize) -> usize {
    std::env::var("MC_UNPACK_FUZZ")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Runs `f` on `rounds` mutations of `seed_bytes`; a panic prints which round
pub fn fuzz(name: &str, seed_bytes: &[u8], rounds: usize, mut f: impl FnMut(&[u8])) {
    for i in 0..rounds {
        let mut r = Rng(0x9E37_79B9_7F4A_7C15 ^ (i as u64 + 1).wrapping_mul(0xD1B5_4A32_D192_ED03));
        let m = mutate(seed_bytes, &mut r);
        if let Err(e) = catch_unwind(AssertUnwindSafe(|| f(&m))) {
            eprintln!("fuzz target {name} panicked in round {i} (seed {:#x})", r.0);
            resume_unwind(e);
        }
    }
}

fn sample_chunk(old: bool) -> ChunkSpec {
    let mut c = ChunkSpec::new(0, 0);
    c.fill((0, 60, 0), (7, 64, 7), "minecraft:stone");
    c.set(
        3,
        70,
        3,
        "minecraft:oak_slab",
        &[("type", "top"), ("waterlogged", "false")],
    );
    c.set(4, 70, 3, "minecraft:grass_block", &[("snowy", "false")]);
    c.set(5, -3, 5, "minecraft:oak_leaves", &[]);
    c.biome_cells.insert((4, 3), "minecraft:desert".into());
    c.old_layout = old;
    c
}

#[test]
fn the_nbt_reader_survives_mutated_documents() {
    for old in [false, true] {
        let doc = document("", &sample_chunk(old).nbt());
        fuzz("nbt", &doc, rounds(4000), |m| {
            let _ = nbt::parse(m);
        });
    }
}

#[test]
fn the_chunk_reader_and_the_block_table_survive_mutated_chunks() {
    let a = world_jar();
    for old in [false, true] {
        let doc = document("", &sample_chunk(old).nbt());
        fuzz("chunk", &doc, rounds(3000), |m| {
            if let Ok((_, root)) = nbt::parse(m) {
                match parse_chunk(&root, None) {
                    Ok(c) => {
                        let mut t = StateTable::new(&a);
                        let mut b = super::biomes::BiomeTable::new(&a);
                        let _ = to_blocks(&c, &mut t, &mut b);
                    }
                    Err(ChunkError::Legacy { .. } | ChunkError::Bad(_)) => {}
                }
            }
        });
    }
}

#[test]
fn the_decompressors_survive_mutated_streams() {
    let data: Vec<u8> = (0..5000u32).map(|i| (i / 7) as u8).collect();
    let real_lz4 = {
        // the real block of compress.rs inside the stream framing
        let mut s = ctests::lz4_stored(&data);
        s.truncate(s.len());
        s
    };
    for (kind, bytes) in [
        (Compression::Zlib, ctests::zlib(&data)),
        (Compression::Gzip, ctests::gzip(&data)),
        (Compression::Lz4, real_lz4),
        (Compression::None, data.clone()),
    ] {
        fuzz(kind.name(), &bytes, rounds(3000), |m| {
            let _ = decompress(kind, m, 1 << 16);
        });
    }
}

#[test]
fn the_region_reader_survives_mutated_files() {
    let mut a = sample_chunk(false);
    a.cx = 0;
    let mut b = sample_chunk(false);
    b.cx = 1;
    b.cz = 0;
    let raw: Vec<(u32, u32, u8, Vec<u8>)> = [&a, &b]
        .iter()
        .map(|c| {
            (
                c.cx as u32,
                c.cz as u32,
                2u8,
                ctests::zlib(&document("", &c.nbt())),
            )
        })
        .collect();
    let region = region_bytes(&raw);
    let dir = temp_dir("fuzz-region");
    let path = dir.join("r.0.0.mca");
    fuzz("region", &region, rounds(400), |m| {
        std::fs::write(&path, m).unwrap();
        if let Ok(mut r) = Region::open(&path) {
            for lz in 0..32 {
                for lx in 0..32 {
                    if r.has_chunk(lx, lz) {
                        let _ = r.read_chunk(lx, lz);
                    }
                }
            }
        }
    });
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_whole_load_survives_mutated_region_files() {
    let a = world_jar();
    let mut c0 = sample_chunk(false);
    c0.cx = 0;
    let mut c1 = sample_chunk(false);
    c1.cx = 1;
    let save = write_world("fuzz-load", "9.9", 9000, &[c0, c1], false);
    let rp = save.join("dimensions/minecraft/overworld/region/r.0.0.mca");
    let original = std::fs::read(&rp).unwrap();
    let w = World::open(&save).unwrap();
    fuzz("load", &original, rounds(300), |m| {
        std::fs::write(&rp, m).unwrap();
        let mut o = BuildOptions::new(Window::region(0, 0));
        o.limits.max_quads = 100_000;
        let _ = build(&a, &w, &o);
    });
    cleanup(&save);
}

#[test]
fn level_dat_survives_mutation() {
    let lv = crate::world::save::tests::level_dat("26.3", 5023);
    fuzz("level.dat", &lv, rounds(2000), |m| {
        let dir = temp_dir("fuzz-level");
        std::fs::write(dir.join("level.dat"), m).unwrap();
        let _ = World::open(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
    });
}
