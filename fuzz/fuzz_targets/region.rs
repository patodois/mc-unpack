#![no_main]
//! A region file (`r.<x>.<z>.mca`) of arbitrary bytes: open it, read every listed chunk, and parse
//! each chunk's NBT. The same bytes are also offered as the external chunk file `c.0.0.mcc`.
use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use mc_unpack::world::{chunk::parse_chunk, nbt, region::Region};

fuzz_target!(|data: &[u8]| {
    static DIR: OnceLock<std::path::PathBuf> = OnceLock::new();
    let dir = DIR.get_or_init(|| mc_unpack_fuzz::scratch("region"));
    let path = dir.join("r.0.0.mca");
    std::fs::write(&path, data).unwrap();
    std::fs::write(dir.join("c.0.0.mcc"), data).unwrap();
    let Ok(mut r) = Region::open(&path) else {
        return;
    };
    let _ = r.chunk_count();
    for lz in 0..32 {
        for lx in 0..32 {
            if !r.has_chunk(lx, lz) {
                continue;
            }
            if let Ok(Some(bytes)) = r.read_chunk(lx, lz) {
                if let Ok((_, root)) = nbt::parse(&bytes) {
                    let _ = parse_chunk(&root, Some((lx as i32, lz as i32)));
                }
            }
        }
    }
});
