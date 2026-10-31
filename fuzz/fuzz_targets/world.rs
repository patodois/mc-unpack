#![no_main]
//! A save folder with a valid `level.dat` and one region file made of the input bytes, meshed
//! through a synthetic one-block jar: the loader, the chunk reader, the culling and the mesher.
use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use mc_unpack::world::mesher::{build, BuildOptions, Window};
use mc_unpack::world::save::World;
use mc_unpack::Assets;

// TAG_Compound "" { Data: { DataVersion: 3700 } } as uncompressed NBT (the reader accepts it)
const LEVEL: &[u8] = b"\x0a\x00\x00\x0a\x00\x04Data\x03\x00\x0bDataVersion\x00\x00\x0e\x74\x00\x00";

fuzz_target!(|data: &[u8]| {
    static ASSETS: OnceLock<Assets> = OnceLock::new();
    static DIR: OnceLock<std::path::PathBuf> = OnceLock::new();
    let assets = ASSETS.get_or_init(|| {
        let png = mc_unpack_fuzz::png(2, 2, |x, y| [x as u8 * 100, y as u8 * 100, 50, 255]);
        Assets::from_jar(mc_unpack_fuzz::mini_jar(&[(
            "assets/minecraft/textures/block/stone.png",
            &png,
        )]))
        .unwrap()
    });
    let dir = DIR.get_or_init(|| {
        let d = mc_unpack_fuzz::scratch("world");
        std::fs::create_dir_all(d.join("region")).unwrap();
        std::fs::write(d.join("level.dat"), LEVEL).unwrap();
        d
    });
    std::fs::write(dir.join("region").join("r.0.0.mca"), data).unwrap();
    let Ok(world) = World::open(dir) else {
        panic!("the fixed level.dat must open");
    };
    let _ = world.regions("overworld");
    let _ = build(assets, &world, &BuildOptions::new(Window::region(0, 0)));
});
