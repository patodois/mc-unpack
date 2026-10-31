#![no_main]
//! `world::compress::decompress`: the first byte picks gzip, zlib, none or LZ4; the rest is the data.
//! The limit is small so a bomb is cheap to hit.
use libfuzzer_sys::fuzz_target;
use mc_unpack::world::compress::{decompress, Compression};

fuzz_target!(|data: &[u8]| {
    let Some((&id, rest)) = data.split_first() else {
        return;
    };
    let Ok(kind) = Compression::from_id(id) else {
        return;
    };
    const LIMIT: usize = 1 << 20;
    if let Ok(out) = decompress(kind, rest, LIMIT) {
        assert!(out.len() <= LIMIT);
    }
});
