#![no_main]
//! The whole asset pipeline on a synthetic jar whose files come from the input: a blockstate, two
//! models (`block/stone` and its parent `block/cube_all`), a texture with its `.mcmeta`.
//! Input: five parts, the first four with a big-endian u16 length in front; the last is the rest.
use libfuzzer_sys::fuzz_target;
use mc_unpack::blockstate::State;
use mc_unpack::jar::Jar;
use mc_unpack::Assets;

fn part<'a>(data: &mut &'a [u8]) -> &'a [u8] {
    if data.len() < 2 {
        return &[];
    }
    let n = (u16::from_be_bytes([data[0], data[1]]) as usize).min(data.len() - 2);
    let (p, rest) = data[2..].split_at(n);
    *data = rest;
    p
}

fuzz_target!(|data: &[u8]| {
    let mut d = data;
    let bs = part(&mut d);
    let model = part(&mut d);
    let parent = part(&mut d);
    let png = part(&mut d);
    let meta = d;
    let bytes = mc_unpack_fuzz::zip_bytes(&[
        ("assets/minecraft/blockstates/stone.json", bs),
        ("assets/minecraft/models/block/stone.json", model),
        ("assets/minecraft/models/block/cube_all.json", parent),
        ("assets/minecraft/textures/block/stone.png", png),
        ("assets/minecraft/textures/block/stone.png.mcmeta", meta),
    ]);
    let Ok(jar) = Jar::from_bytes(bytes) else {
        return;
    };
    let Ok(a) = Assets::from_jar(jar) else {
        return;
    };
    let _ = a.default_state("stone");
    for s in ["", "facing=east"] {
        if let Ok(st) = State::parse(s) {
            let _ = a.bake_block("stone", &st);
        }
    }
    let _ = a.bake_id("block/stone");
    let _ = a.bake_id("block/cube_all");
    let _ = a.texture("block/stone");
    let _ = a.classify_block("stone");
});
