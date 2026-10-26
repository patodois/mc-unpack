//! Tests against real saves of the machine they run on. Each skips (and says so) when no save of
//! the needed version is found, so the suite passes anywhere; **no world data is in this repository**
//! and nothing is ever written to a save: the tests read it, and the damaged copies the fuzz test
//! makes go to a temp folder.
//!
//! Saves are looked for where `mc-unpack worlds` looks (Prism Launcher instances, the vanilla
//! launcher, `MC_UNPACK_SAVES`), jars where `mc-unpack versions` looks.

use std::path::PathBuf;

use mc_unpack::assets::Assets;
use mc_unpack::install::find_jars;
use mc_unpack::world::biomes::BiomeTable;
use mc_unpack::world::blocks::{StateTable, AIR};
use mc_unpack::world::chunk::{parse_chunk, unpack};
use mc_unpack::world::grid::{to_blocks, BlockGrid};
use mc_unpack::world::mesher::{build, BuildOptions, Window};
use mc_unpack::world::nbt::{self, Tag};
use mc_unpack::world::region::Region;
use mc_unpack::world::save::{discover_saves, World};

/// The saves whose data version is `version`'s, with the jar that reads them
fn saves_of(version: &str) -> Vec<(World, Assets)> {
    let Some(jar) = find_jars().into_iter().find(|j| j.version == version) else {
        eprintln!("skipping: Minecraft {version} is not installed here");
        return Vec::new();
    };
    let Ok(assets) = Assets::open(&jar.path) else {
        eprintln!("skipping: Minecraft {version} is not readable");
        return Vec::new();
    };
    let mut out = Vec::new();
    for s in discover_saves() {
        if let Ok(w) = World::open(&s.path) {
            if w.level.version_name.as_deref() == Some(version) {
                out.push((w, Assets::open(&jar.path).unwrap()));
            }
        }
    }
    if out.is_empty() {
        eprintln!("skipping: no save made by Minecraft {version} found (create one with that version to run this test)");
    }
    drop(assets);
    out
}

macro_rules! saves {
    ($v:expr) => {{
        let s = saves_of($v);
        if s.is_empty() {
            return;
        }
        s
    }};
}

#[test]
fn real_26_3_regions_load_without_failures_and_account_for_every_block() {
    for (w, a) in saves!("26.3") {
        for dim in w.dimensions() {
            for (rx, rz) in w.regions(&dim).unwrap() {
                let mut o = BuildOptions::new(Window::region(rx, rz));
                o.dimension = dim.clone();
                let (mesh, r) = build(&a, &w, &o)
                    .unwrap_or_else(|e| panic!("{} {dim} {rx},{rz}: {e:#}", w.name()));
                let ctx = format!("{} {dim} r.{rx}.{rz}", w.dir.display());
                assert_eq!(r.load.failed_count, 0, "{ctx}: {:?}", r.load.failed);
                assert_eq!(r.load.legacy, 0, "{ctx}");
                assert!(
                    r.unknown.is_empty(),
                    "{ctx}: blocks the 26.3 jar does not know: {:?}",
                    r.unknown
                );
                // every block is drawn, left out with a reason, or not in the jar
                let left: u64 = r.left_out.iter().map(|l| l.blocks).sum();
                let unknown: u64 = r.unknown.iter().map(|l| l.blocks).sum();
                assert_eq!(r.blocks_drawn + left + unknown, r.blocks, "{ctx}");
                assert!(r.left_out.iter().all(|l| !l.reason.is_empty()), "{ctx}");
                assert_eq!(mesh.quad_count(), r.quads, "{ctx}");
                assert_eq!(
                    r.load.listed,
                    r.load.loaded + r.load.unfinished.values().sum::<usize>(),
                    "{ctx}"
                );
            }
        }
    }
}

/// The game stores, for every finished chunk, the height of the highest block that is not air in
/// each column (`Heightmaps.WORLD_SURFACE`). It is an independent record of where the blocks are,
/// written by the game and not by this crate, so the chunks it decodes must agree with it for every
/// column: this checks the packing, the section order, negative heights and the x/z order at once.
#[test]
fn the_blocks_agree_with_the_heightmaps_the_game_saved() {
    for (w, a) in saves!("26.3") {
        let dir = w.region_dir("overworld").unwrap();
        let mut checked = 0usize;
        for (rx, rz) in w.regions("overworld").unwrap() {
            let mut r = Region::open(&dir.join(format!("r.{rx}.{rz}.mca"))).unwrap();
            let mut table = StateTable::new(&a);
            let mut biomes = BiomeTable::new(&a);
            for lz in 0..32 {
                for lx in 0..32 {
                    let Some(raw) = r.read_chunk(lx, lz).unwrap() else {
                        continue;
                    };
                    let (_, root) = nbt::parse(&raw).unwrap();
                    let (cx, cz) = (rx * 32 + lx as i32, rz * 32 + lz as i32);
                    let chunk = parse_chunk(&root, Some((cx, cz))).unwrap();
                    if chunk.status.as_deref() != Some("minecraft:full") {
                        continue;
                    }
                    let blocks = to_blocks(&chunk, &mut table, &mut biomes).unwrap();
                    let mut grid = BlockGrid::new(cx, cz, cx, cz);
                    grid.set(cx, cz, blocks);
                    let min_y = root.get("yPos").and_then(Tag::as_i64).unwrap() as i32 * 16;
                    let hm = root
                        .get("Heightmaps")
                        .and_then(|h| h.get("WORLD_SURFACE"))
                        .and_then(Tag::as_long_array)
                        .unwrap();
                    // 9 bits an entry for a world 384 high, index x + z * 16
                    let heights = unpack(hm, 9, 256).unwrap();
                    for z in 0..16i32 {
                        for x in 0..16i32 {
                            let top = (min_y..min_y + 384)
                                .rev()
                                .find(|&y| grid.get(cx * 16 + x, y, cz * 16 + z) != AIR)
                                .map(|y| y + 1 - min_y)
                                .unwrap_or(0);
                            assert_eq!(
                                top,
                                heights[(x + z * 16) as usize] as i32,
                                "chunk {cx},{cz} column {x},{z} of {}",
                                w.dir.display()
                            );
                        }
                    }
                    checked += 1;
                }
            }
        }
        assert!(checked > 0, "{}: no finished chunk", w.dir.display());
        eprintln!("{}: {checked} chunks agree with their heightmaps", w.name());
    }
}

#[test]
fn only_the_item_definitions_say_which_blocks_take_the_grass_colormap() {
    use mc_unpack::tint::ColormapKind;
    use mc_unpack::world::blocks::data_tint;
    for v in ["26.2", "26.3"] {
        let Some(jar) = find_jars().into_iter().find(|j| j.version == v) else {
            eprintln!("skipping: Minecraft {v} is not installed here");
            continue;
        };
        let a = Assets::open(&jar.path).unwrap();
        fn grass(v: &serde_json::Value) -> bool {
            match v {
                serde_json::Value::Object(m) => {
                    m.get("type").and_then(|t| t.as_str()) == Some("minecraft:grass")
                        || m.values().any(grass)
                }
                serde_json::Value::Array(l) => l.iter().any(grass),
                _ => false,
            }
        }
        let mut grass_items = Vec::new();
        for n in a.item_names() {
            let Ok(d) = a.item_definition(&n) else {
                continue;
            };
            // exactly the items whose definition has a grass tint are the blocks that get one
            assert_eq!(
                data_tint(&a, &n),
                grass(&d).then_some(ColormapKind::Grass),
                "{v}: {n}"
            );
            if grass(&d) {
                grass_items.push(n);
            }
        }
        assert!(grass_items.len() >= 6, "{v}: {grass_items:?}");
        // leaves and vines have a constant item tint: the data does not say a colormap
        for n in ["oak_leaves", "vine", "birch_leaves"] {
            assert_eq!(data_tint(&a, n), None, "{v}: {n}");
        }
    }
    // a jar from before the item definitions says nothing, grass included
    if let Some(jar) = find_jars().into_iter().find(|j| j.version == "1.21.1") {
        let a = Assets::open(&jar.path).unwrap();
        assert_eq!(data_tint(&a, "grass_block"), None);
    }
}

#[test]
fn a_world_of_numeric_ids_is_said_to_be_left_out_not_guessed() {
    // a 1.12.2 save, read with the 1.12.2 jar: every chunk is numeric ids
    let Some(jar) = find_jars().into_iter().find(|j| j.version == "1.12.2") else {
        eprintln!("skipping: Minecraft 1.12.2 is not installed here");
        return;
    };
    let a = Assets::open(&jar.path).unwrap();
    let mut seen = 0;
    for s in discover_saves() {
        let Ok(w) = World::open(&s.path) else {
            continue;
        };
        if w.level.version_name.as_deref() != Some("1.12.2") {
            continue;
        }
        for (rx, rz) in w.regions("overworld").unwrap().into_iter().take(2) {
            let (mesh, r) = build(&a, &w, &BuildOptions::new(Window::region(rx, rz))).unwrap();
            assert!(mesh.chunks.is_empty());
            assert_eq!(r.quads, 0);
            assert!(r.load.legacy > 0);
            assert_eq!(r.load.failed_count, 0, "{:?}", r.load.failed);
            assert!(r.lines().join("\n").contains("numeric block ids"));
            seen += 1;
        }
    }
    if seen == 0 {
        eprintln!("skipping: no 1.12.2 save found");
    }
}

#[test]
fn a_save_of_a_version_with_no_models_is_refused_with_the_reason() {
    let Some(jar) = find_jars().into_iter().find(|j| j.version == "1.7.10") else {
        eprintln!("skipping: Minecraft 1.7.10 is not installed here");
        return;
    };
    let a = Assets::open(&jar.path).unwrap();
    for s in discover_saves() {
        let Ok(w) = World::open(&s.path) else {
            continue;
        };
        let Some((rx, rz)) = w
            .regions("overworld")
            .ok()
            .and_then(|r| r.into_iter().next())
        else {
            continue;
        };
        if w.level.data_version.is_none() {
            let e = build(&a, &w, &BuildOptions::new(Window::region(rx, rz)))
                .unwrap_err()
                .to_string();
            assert!(e.contains("textures only"), "{e}");
            return;
        }
    }
    eprintln!("skipping: no save from before 1.9 found");
}

#[cfg(feature = "bridge")]
#[test]
fn real_regions_are_valid_scenes_the_checker_accepts() {
    use mc_unpack::bridge::Minecraft;
    use rewrite_bridge::conformance::{check_scene, Expect};
    use rewrite_bridge::{AssetKind, Provider};
    let Some(jar) = find_jars().into_iter().find(|j| j.version == "26.3") else {
        eprintln!("skipping: Minecraft 26.3 is not installed here");
        return;
    };
    let mc = Minecraft::open(&jar.path).unwrap().with_discovered_saves();
    let levels: Vec<_> = mc
        .list("world/")
        .unwrap()
        .into_iter()
        .filter(|l| l.kind == AssetKind::Level)
        .collect();
    // the worlds of 1.12.2 and before are not offered
    assert!(
        levels
            .iter()
            .all(|l| !l.id.contains("world/1.12.2/") && !l.id.contains("world/1.7.10/")),
        "{levels:?}"
    );
    let mut checked = 0;
    for l in levels.iter().filter(|l| l.name.contains("Minecraft 26.3")) {
        let (scene, report) = mc.load_world_report(&l.id).unwrap();
        scene.validate().unwrap();
        let rep = check_scene(&scene, &Expect::for_kind(AssetKind::Level));
        // an empty region (only unfinished chunks) is a warning the checker gives, and nothing else is
        if scene.instances.is_empty() {
            assert!(
                report.load.listed > 0 && report.load.loaded == 0,
                "{}",
                l.id
            );
            continue;
        }
        assert!(
            rep.is_clean(),
            "{}: {}",
            l.id,
            rep.render(rewrite_bridge::conformance::Severity::Info)
        );
        checked += 1;
    }
    if checked == 0 {
        eprintln!("skipping: no 26.3 save found");
    }
}

// ---- hostile files, made from a real one ----

fn xorshift(s: &mut u64) -> u64 {
    *s ^= *s >> 12;
    *s ^= *s << 25;
    *s ^= *s >> 27;
    s.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

fn mutate(src: &[u8], seed: &mut u64) -> Vec<u8> {
    let mut b = src.to_vec();
    for _ in 0..1 + (xorshift(seed) % 6) {
        let n = b.len();
        match xorshift(seed) % 6 {
            0 => {
                let i = (xorshift(seed) as usize) % n;
                b[i] ^= 1 << (xorshift(seed) % 8);
            }
            1 => {
                let i = (xorshift(seed) as usize) % n;
                b[i] = xorshift(seed) as u8;
            }
            2 => {
                // somewhere in the 8 KiB table, where the offsets and lengths are
                let i = (xorshift(seed) as usize) % 8192.min(n);
                b[i] = xorshift(seed) as u8;
            }
            3 => b.truncate((xorshift(seed) as usize) % n),
            4 => {
                // a chunk's length field set to something extreme
                let i = ((xorshift(seed) as usize) % (n / 4096).max(1)) * 4096;
                if i + 4 <= b.len() {
                    b[i..i + 4].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xF0]);
                }
            }
            _ => {
                let from = (xorshift(seed) as usize) % n;
                let len = 1 + (xorshift(seed) as usize) % 64.min(n - from);
                let to = (xorshift(seed) as usize) % (n - len + 1);
                let piece = b[from..from + len].to_vec();
                b[to..to + len].copy_from_slice(&piece);
            }
        }
    }
    b
}

#[test]
fn damaged_copies_of_a_real_region_never_panic() {
    let rounds: usize = std::env::var("MC_UNPACK_FUZZ")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(40);
    for (w, a) in saves!("26.3") {
        let dir = w.region_dir("overworld").unwrap();
        // the biggest region: the most chunks of finished terrain to damage
        let Some((rx, rz)) = w
            .regions("overworld")
            .unwrap()
            .into_iter()
            .max_by_key(|(x, z)| {
                std::fs::metadata(dir.join(format!("r.{x}.{z}.mca")))
                    .map(|m| m.len())
                    .unwrap_or(0)
            })
        else {
            continue;
        };
        let original = std::fs::read(dir.join(format!("r.{rx}.{rz}.mca"))).unwrap();
        // a throwaway save of its own: a copy of level.dat and the damaged region
        let tmp: PathBuf =
            std::env::temp_dir().join(format!("mc-unpack-realfuzz-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let rd = tmp.join("dimensions/minecraft/overworld/region");
        std::fs::create_dir_all(&rd).unwrap();
        std::fs::copy(w.dir.join("level.dat"), tmp.join("level.dat")).unwrap();
        let copy = World::open(&tmp).unwrap();
        let mut seed = 0x1234_5678_9ABC_DEF1u64;
        let (mut ok, mut err) = (0, 0);
        for _ in 0..rounds {
            let m = mutate(&original, &mut seed);
            std::fs::write(rd.join(format!("r.{rx}.{rz}.mca")), &m).unwrap();
            let mut o = BuildOptions::new(Window::region(rx, rz));
            o.limits.max_quads = 2_000_000;
            match build(&a, &copy, &o) {
                Ok(_) => ok += 1,
                Err(_) => err += 1,
            }
        }
        eprintln!("{} r.{rx}.{rz}: {rounds} damaged copies, {ok} loaded (some chunks may have failed), {err} refused", w.name());
        std::fs::remove_dir_all(&tmp).unwrap();
        return;
    }
}

#[test]
fn damaged_real_chunks_never_panic() {
    let rounds: usize = std::env::var("MC_UNPACK_FUZZ")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    for (w, a) in saves!("26.3") {
        let dir = w.region_dir("overworld").unwrap();
        // a finished chunk with the most sections' worth of data: the first one that is full
        let mut found = None;
        'regions: for (rx, rz) in w.regions("overworld").unwrap() {
            let mut r = Region::open(&dir.join(format!("r.{rx}.{rz}.mca"))).unwrap();
            for lz in 0..32 {
                for lx in 0..32 {
                    if let Some(raw) = r.read_chunk(lx, lz).unwrap() {
                        let (_, root) = nbt::parse(&raw).unwrap();
                        if root.get("Status").and_then(Tag::as_str) == Some("minecraft:full") {
                            found = Some(raw);
                            break 'regions;
                        }
                    }
                }
            }
        }
        let Some(raw) = found else { continue };
        let mut table = StateTable::new(&a);
        let mut biomes = BiomeTable::new(&a);
        let mut seed = 0xFEED_FACE_CAFE_BEEFu64;
        let (mut parsed, mut chunks, mut blocks) = (0, 0, 0);
        for _ in 0..rounds {
            let mut m = raw.clone();
            for _ in 0..1 + (xorshift(&mut seed) % 5) {
                let n = m.len();
                match xorshift(&mut seed) % 4 {
                    0 => {
                        let i = (xorshift(&mut seed) as usize) % n;
                        m[i] ^= 1 << (xorshift(&mut seed) % 8);
                    }
                    1 => {
                        let i = (xorshift(&mut seed) as usize) % n;
                        m[i] = xorshift(&mut seed) as u8;
                    }
                    2 => {
                        let i = (xorshift(&mut seed) as usize) % (n - 4);
                        let v = [0u32, 1, 0x7FFF_FFFF, 0xFFFF_FFFF, 4096]
                            [(xorshift(&mut seed) % 5) as usize];
                        m[i..i + 4].copy_from_slice(&v.to_be_bytes());
                    }
                    _ => m.truncate(1 + (xorshift(&mut seed) as usize) % n),
                }
            }
            let Ok((_, root)) = nbt::parse(&m) else {
                continue;
            };
            parsed += 1;
            let Ok(c) = parse_chunk(&root, None) else {
                continue;
            };
            chunks += 1;
            if to_blocks(&c, &mut table, &mut biomes).is_ok() {
                blocks += 1;
            }
        }
        eprintln!("{rounds} damaged chunks: {parsed} still NBT, {chunks} still chunks, {blocks} still blocks");
        return;
    }
}
