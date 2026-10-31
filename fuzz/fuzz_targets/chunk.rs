#![no_main]
//! A chunk's NBT (parsed from arbitrary bytes) to sections, then to block ids through a synthetic
//! one-block jar; and the bit unpacking on its own.
use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use mc_unpack::world::biomes::BiomeTable;
use mc_unpack::world::blocks::StateTable;
use mc_unpack::world::chunk::{parse_chunk, unpack};
use mc_unpack::world::grid::to_blocks;
use mc_unpack::world::nbt;
use mc_unpack::Assets;

fuzz_target!(|data: &[u8]| {
    static ASSETS: OnceLock<Assets> = OnceLock::new();
    let assets = ASSETS.get_or_init(|| Assets::from_jar(mc_unpack_fuzz::mini_jar(&[])).unwrap());
    if data.len() >= 4 && data[0] == 0xFF {
        // raw bit unpacking: bits, count, then the longs
        let bits = data[1] as usize;
        let count = u16::from_be_bytes([data[2], data[3]]) as usize;
        let longs: Vec<i64> = data[4..]
            .chunks(8)
            .map(|c| {
                let mut b = [0u8; 8];
                b[..c.len()].copy_from_slice(c);
                i64::from_be_bytes(b)
            })
            .collect();
        let _ = unpack(&longs, bits, count);
        return;
    }
    let Ok((_, root)) = nbt::parse(data) else {
        return;
    };
    for expect in [None, Some((0, 0))] {
        if let Ok(chunk) = parse_chunk(&root, expect) {
            let mut table = StateTable::new(assets);
            let mut biomes = BiomeTable::new(assets);
            let _ = to_blocks(&chunk, &mut table, &mut biomes);
        }
    }
});
