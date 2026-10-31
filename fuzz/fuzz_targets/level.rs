#![no_main]
//! `level.dat`: the NBT reader plus `parse_level`, and the file reader (gzip, or plain NBT)
use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use mc_unpack::world::level::{parse_level, read_level_dat};
use mc_unpack::world::nbt;

fuzz_target!(|data: &[u8]| {
    if let Ok((_, root)) = nbt::parse(data) {
        let _ = parse_level(&root);
    }
    static DIR: OnceLock<std::path::PathBuf> = OnceLock::new();
    let dir = DIR.get_or_init(|| mc_unpack_fuzz::scratch("level"));
    let path = dir.join("level.dat");
    std::fs::write(&path, data).unwrap();
    let _ = read_level_dat(&path);
});
