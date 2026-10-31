//! Writes the seed corpora under `fuzz/corpus/<target>/`: small synthetic inputs, built here, no
//! game data. Files are named by the FNV-1a hash of their content, with no extension (the
//! repository's public-tree gate refuses `level.dat`, `.mca`, `.jar` and the like by name).
//!
//! Run from the `fuzz` directory:  cargo run --release --bin gen_seeds

use std::io::Write;
use std::path::Path;

use mc_unpack_fuzz::{png, zip_bytes};

// ---- an NBT writer (big-endian, as the format is) ----
fn s(out: &mut Vec<u8>, t: &str) {
    out.extend_from_slice(&(t.len() as u16).to_be_bytes());
    out.extend_from_slice(t.as_bytes());
}
fn named(out: &mut Vec<u8>, ty: u8, name: &str) {
    out.push(ty);
    s(out, name);
}
fn end(out: &mut Vec<u8>) {
    out.push(0);
}
fn int(out: &mut Vec<u8>, name: &str, v: i32) {
    named(out, 3, name);
    out.extend_from_slice(&v.to_be_bytes());
}
fn string(out: &mut Vec<u8>, name: &str, v: &str) {
    named(out, 8, name);
    s(out, v);
}

fn pack_padded(idx: &[u16], bits: usize) -> Vec<i64> {
    let per = 64 / bits;
    idx.chunks(per)
        .map(|c| {
            let mut l = 0u64;
            for (i, &v) in c.iter().enumerate() {
                l |= (v as u64) << (i * bits);
            }
            l as i64
        })
        .collect()
}

fn long_array(out: &mut Vec<u8>, name: &str, v: &[i64]) {
    named(out, 12, name);
    out.extend_from_slice(&(v.len() as i32).to_be_bytes());
    for x in v {
        out.extend_from_slice(&x.to_be_bytes());
    }
}

fn palette_entry(out: &mut Vec<u8>, name: &str, props: &[(&str, &str)]) {
    string(out, "Name", name);
    if !props.is_empty() {
        named(out, 10, "Properties");
        for (k, v) in props {
            string(out, k, v);
        }
        end(out);
    }
}

/// A 1.18+ chunk at (cx, cz): sections 0 (uniform stone) and 1 (two states, dense)
fn chunk_nbt(cx: i32, cz: i32, status: &str) -> Vec<u8> {
    let mut o = Vec::new();
    named(&mut o, 10, "");
    int(&mut o, "DataVersion", 3700);
    int(&mut o, "xPos", cx);
    int(&mut o, "zPos", cz);
    int(&mut o, "yPos", -4);
    string(&mut o, "Status", status);
    named(&mut o, 9, "sections");
    o.push(10);
    o.extend_from_slice(&2i32.to_be_bytes());
    // section 0: a single-entry palette, no data
    {
        named(&mut o, 1, "Y");
        o.push(0);
        named(&mut o, 10, "block_states");
        named(&mut o, 9, "palette");
        o.push(10);
        o.extend_from_slice(&1i32.to_be_bytes());
        palette_entry(&mut o, "minecraft:stone", &[]);
        end(&mut o);
        end(&mut o);
        end(&mut o);
    }
    // section 1: stone and a stair, 4 bits per entry
    {
        named(&mut o, 1, "Y");
        o.push(1);
        named(&mut o, 10, "block_states");
        named(&mut o, 9, "palette");
        o.push(10);
        o.extend_from_slice(&2i32.to_be_bytes());
        palette_entry(&mut o, "minecraft:stone", &[]);
        end(&mut o);
        palette_entry(
            &mut o,
            "minecraft:oak_stairs",
            &[("facing", "east"), ("half", "bottom")],
        );
        end(&mut o);
        let idx: Vec<u16> = (0..4096).map(|i| (i % 7 == 0) as u16).collect();
        long_array(&mut o, "data", &pack_padded(&idx, 4));
        end(&mut o);
        end(&mut o);
    }
    end(&mut o);
    o
}

fn level_nbt(with_new_spawn: bool) -> Vec<u8> {
    let mut o = Vec::new();
    named(&mut o, 10, "");
    named(&mut o, 10, "Data");
    int(&mut o, "DataVersion", 3700);
    string(&mut o, "LevelName", "Synthetic");
    named(&mut o, 10, "Version");
    string(&mut o, "Name", "1.20.4");
    named(&mut o, 1, "Snapshot");
    o.push(0);
    end(&mut o);
    if with_new_spawn {
        named(&mut o, 10, "spawn");
        named(&mut o, 11, "pos");
        o.extend_from_slice(&3i32.to_be_bytes());
        for v in [1i32, 64, -2] {
            o.extend_from_slice(&v.to_be_bytes());
        }
        string(&mut o, "dimension", "minecraft:overworld");
        end(&mut o);
    } else {
        int(&mut o, "SpawnX", 8);
        int(&mut o, "SpawnY", 70);
        int(&mut o, "SpawnZ", 8);
    }
    end(&mut o);
    end(&mut o);
    o
}

// ---- compression ----
fn zlib(d: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(d).unwrap();
    e.finish().unwrap()
}
fn gzip(d: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(d).unwrap();
    e.finish().unwrap()
}

/// The lz4-java block stream: `data` in raw blocks (method 0x10) and in one literal-only
/// compressed block (method 0x20), then the end mark. The layout is the one `compress.rs` reads
/// (magic, method|level, compressed and decompressed lengths, checksum), all little-endian.
fn lz4_stream(d: &[u8], compressed: bool) -> Vec<u8> {
    use mc_unpack::world::compress::xxh32;
    let header = |out: &mut Vec<u8>, method: u8, clen: usize, dlen: usize, check: u32| {
        out.extend_from_slice(b"LZ4Block");
        out.push(method | 9);
        out.extend_from_slice(&(clen as i32).to_le_bytes());
        out.extend_from_slice(&(dlen as i32).to_le_bytes());
        out.extend_from_slice(&check.to_le_bytes());
    };
    let mut o = Vec::new();
    for chunk in d.chunks(1000) {
        let sum = xxh32(chunk, 0x9747_B28C) & 0x0FFF_FFFF;
        if compressed {
            // one sequence of literals and no match
            let n = chunk.len();
            let mut block = vec![0xF0];
            let mut r = n - 15;
            while r >= 255 {
                block.push(255);
                r -= 255;
            }
            block.push(r as u8);
            block.extend_from_slice(chunk);
            header(&mut o, 0x20, block.len(), n, sum);
            o.extend_from_slice(&block);
        } else {
            header(&mut o, 0x10, chunk.len(), chunk.len(), sum);
            o.extend_from_slice(chunk);
        }
    }
    header(&mut o, 0x10, 0, 0, 0);
    o
}

/// A region file with the given chunks: (local x, local z, compression id, payload)
fn region(chunks: &[(u32, u32, u8, Vec<u8>)]) -> Vec<u8> {
    let mut table = vec![0u8; 4096];
    let mut body = Vec::new();
    let mut sector = 2u32;
    for (lx, lz, comp, payload) in chunks {
        let len = payload.len() as u32 + 1;
        let total = 4 + len as usize;
        let sectors = total.div_ceil(4096);
        let e = (sector << 8) | sectors as u32;
        let at = ((lx + lz * 32) * 4) as usize;
        table[at..at + 4].copy_from_slice(&e.to_be_bytes());
        body.extend_from_slice(&len.to_be_bytes());
        body.push(*comp);
        body.extend_from_slice(payload);
        body.resize(body.len().div_ceil(4096) * 4096, 0);
        sector += sectors as u32;
    }
    let mut out = table;
    out.extend_from_slice(&[0u8; 4096]);
    out.extend_from_slice(&body);
    out
}

fn put(root: &Path, target: &str, bytes: &[u8]) {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    let dir = root.join(target);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{h:016x}")), bytes).unwrap();
}

fn part(out: &mut Vec<u8>, p: &[u8]) {
    out.extend_from_slice(&(p.len() as u16).to_be_bytes());
    out.extend_from_slice(p);
}

fn main() {
    let root = Path::new("corpus");
    let chunk = chunk_nbt(0, 0, "minecraft:full");
    let level = level_nbt(false);
    let level_new = level_nbt(true);

    // nbt_parse
    for b in [&chunk, &level, &level_new] {
        put(root, "nbt_parse", b);
    }
    put(root, "nbt_parse", &[10, 0, 0, 0]);

    // decompress: id byte + data
    let raw = chunk.clone();
    let with = |id: u8, d: &[u8]| {
        let mut v = vec![id];
        v.extend_from_slice(d);
        v
    };
    put(root, "decompress", &with(1, &gzip(&raw)));
    put(root, "decompress", &with(2, &zlib(&raw)));
    put(root, "decompress", &with(3, &raw));
    put(root, "decompress", &with(4, &lz4_stream(&raw, false)));
    put(root, "decompress", &with(4, &lz4_stream(&raw, true)));

    // region: one or two chunks, each compression
    let r1 = region(&[(0, 0, 2, zlib(&chunk))]);
    put(root, "region", &r1);
    put(root, "region", &region(&[(0, 0, 1, gzip(&chunk))]));
    put(root, "region", &region(&[(0, 0, 3, chunk.clone())]));
    put(
        root,
        "region",
        &region(&[(0, 0, 4, lz4_stream(&chunk, false))]),
    );
    put(
        root,
        "region",
        &region(&[
            (0, 0, 2, zlib(&chunk)),
            (1, 0, 2, zlib(&chunk_nbt(1, 0, "minecraft:full"))),
        ]),
    );
    put(root, "region", &region(&[(0, 0, 0x80 | 2, vec![])]));
    put(root, "region", &[0u8; 8192]);

    // world: the same regions
    put(root, "world", &r1);
    put(
        root,
        "world",
        &region(&[
            (0, 0, 2, zlib(&chunk)),
            (1, 0, 2, zlib(&chunk_nbt(1, 0, "minecraft:full"))),
        ]),
    );
    put(root, "world", &region(&[(0, 0, 1, gzip(&chunk))]));

    // chunk: NBT, plus the raw unpack form (0xFF, bits, count, longs)
    put(root, "chunk", &chunk);
    put(root, "chunk", &chunk_nbt(0, 0, "minecraft:features"));
    let mut u = vec![0xFF, 4, 0x10, 0x00];
    for l in pack_padded(&(0..4096).map(|i| (i % 16) as u16).collect::<Vec<_>>(), 4) {
        u.extend_from_slice(&l.to_be_bytes());
    }
    put(root, "chunk", &u);

    // level
    put(root, "level", &level);
    put(root, "level", &level_new);
    put(root, "level", &gzip(&level));

    // jar: zips
    let stone_png = png(2, 2, |x, y| [x as u8 * 90, y as u8 * 90, 40, 255]);
    let bs: &[u8] = br#"{"variants":{"":{"model":"minecraft:block/stone"}}}"#;
    let model: &[u8] =
        br##"{"parent":"minecraft:block/cube_all","textures":{"all":"minecraft:block/stone"}}"##;
    put(
        root,
        "jar",
        &zip_bytes(&[
            (
                "version.json",
                br#"{"id":"1.20.4","world_version":3700,"pack_version":22}"#,
            ),
            ("assets/minecraft/blockstates/stone.json", bs),
            ("assets/minecraft/models/block/stone.json", model),
            ("assets/minecraft/textures/block/stone.png", &stone_png),
        ]),
    );
    put(root, "jar", &zip_bytes(&[]));
    put(root, "jar", &zip_bytes(&[("terrain.png", &stone_png)]));

    // png: u16 len, png, mcmeta
    let sheet = png(2, 4, |x, y| [x as u8 * 50, y as u8 * 50, 90, 255]);
    let mut p = Vec::new();
    part(&mut p, &sheet);
    p.extend_from_slice(br#"{"animation":{"frametime":2,"frames":[0,{"index":1,"time":4}]}}"#);
    put(root, "png", &p);
    let mut p = Vec::new();
    part(&mut p, &stone_png);
    put(root, "png", &p);

    // json: selector byte + text
    let j = |sel: u8, t: &[u8]| {
        let mut v = vec![sel];
        v.extend_from_slice(t);
        v
    };
    put(root, "json", &j(0, bs));
    put(
        root,
        "json",
        &j(
            0,
            br#"{"multipart":[{"apply":{"model":"a"}},{"when":{"OR":[{"north":"true"},{"east":"!false"}]},"apply":[{"model":"b","y":90,"weight":2}]}]}"#,
        ),
    );
    put(
        root,
        "json",
        &j(
            1,
            br##"{"parent":"block/block","textures":{"all":"#p"},"elements":[{"from":[0,0,0],"to":[16,8,16],"rotation":{"origin":[8,8,8],"axis":"y","angle":22.5},"faces":{"up":{"texture":"#all","uv":[0,0,16,16],"tintindex":0}}}]}"##,
        ),
    );
    put(
        root,
        "json",
        &j(
            2,
            br##"{"temperature":0.8,"downfall":0.4,"effects":{"grass_color":"#79c05a","water_color":4159204}}"##,
        ),
    );
    put(
        root,
        "json",
        &j(3, br#"{"model":{"type":"minecraft:condition","on_false":{"type":"minecraft:model","model":"item/x"}}}"#),
    );
    put(
        root,
        "json",
        &j(4, br#"{"animation":{"frametime":3,"width":2,"height":2}}"#),
    );
    put(root, "json", &j(5, b"facing=east,half=bottom"));
    put(root, "json", &j(6, b"minecraft:block/stone?facing=east"));
    put(root, "json", &j(7, b"#79c05a"));
    put(root, "json", &j(8, b"/home/u/versions/1.12.2/1.12.2.jar"));

    // assets: bs, model, parent, png, mcmeta
    let parent: &[u8] = br##"{"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{"up":{"texture":"#all","cullface":"up"},"north":{"texture":"#all"}}}]}"##;
    let mut a = Vec::new();
    part(&mut a, bs);
    part(&mut a, model);
    part(&mut a, parent);
    part(&mut a, &stone_png);
    a.extend_from_slice(br#"{"animation":{"frametime":2}}"#);
    put(root, "assets", &a);
}
