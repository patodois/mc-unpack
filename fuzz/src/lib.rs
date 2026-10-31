//! Shared helpers of the fuzz targets: a tiny synthetic jar, and a scratch directory.
//! No game data anywhere: every byte here is built by this file.

use std::io::{Cursor, Write};
use std::path::PathBuf;

use mc_unpack::jar::Jar;

/// A stored zip of `files`
pub fn zip_bytes(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (n, d) in files {
            w.start_file(*n, opts).unwrap();
            w.write_all(d).unwrap();
        }
        w.finish().unwrap();
    }
    buf.into_inner()
}

/// A jar of the 1.13+ layout with one cube block `stone`, one model, one 2 x 2 texture, and the
/// extra entries given
pub fn mini_jar(extra: &[(&str, &[u8])]) -> Jar {
    let mut files: Vec<(&str, &[u8])> = vec![
        (
            "assets/minecraft/blockstates/stone.json",
            br#"{"variants":{"":{"model":"minecraft:block/stone"}}}"#,
        ),
        (
            "assets/minecraft/models/block/stone.json",
            br#"{"parent":"minecraft:block/cube_all","textures":{"all":"minecraft:block/stone"}}"#,
        ),
        (
            "assets/minecraft/models/block/cube_all.json",
            br##"{"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{
"down":{"texture":"#all","cullface":"down"},"up":{"texture":"#all","cullface":"up"},
"north":{"texture":"#all","cullface":"north"},"south":{"texture":"#all","cullface":"south"},
"west":{"texture":"#all","cullface":"west"},"east":{"texture":"#all","cullface":"east"}}}]}"##,
        ),
    ];
    files.extend_from_slice(extra);
    Jar::from_bytes(zip_bytes(&files)).expect("the synthetic jar is a valid zip")
}

/// A fresh scratch directory for this process and `tag` (in memory when the system has /dev/shm)
pub fn scratch(tag: &str) -> PathBuf {
    let base = if std::path::Path::new("/dev/shm").is_dir() {
        PathBuf::from("/dev/shm")
    } else {
        std::env::temp_dir()
    };
    let d = base.join(format!("mc-unpack-fuzz-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A `w` x `h` PNG whose pixels are `f(x, y)`, made by the crate's own encoder
pub fn png(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
    let mut data = Vec::new();
    for y in 0..h {
        for x in 0..w {
            data.extend_from_slice(&f(x, y));
        }
    }
    mc_unpack::texture::encode_png(&mc_unpack::texture::Rgba {
        width: w,
        height: h,
        data,
    })
    .unwrap()
}
