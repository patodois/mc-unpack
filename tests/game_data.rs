//! Tests against real client jars. Each skips (and says so) when the jar is not installed, so
//! the suite passes on a machine, or a CI runner, that has none. No game data is in this repository.
//!
//! The jars are looked for where `mc-unpack versions` looks (Prism Launcher, the vanilla launcher,
//! `MC_UNPACK_PRISM_DIR`, `MC_UNPACK_MINECRAFT_DIR`, `MC_UNPACK_JAR`).

#![allow(clippy::chunks_exact_to_as_chunks)]

use mc_unpack::assets::{Assets, LeftOut};
use mc_unpack::bake::Quad;
use mc_unpack::blockstate::State;
use mc_unpack::check;
use mc_unpack::install::find_jars;
use mc_unpack::model::Dir;
use mc_unpack::tint::{ColormapKind, Tint};
use mc_unpack::version::{Layout, Support};

fn open(version: &str) -> Option<Assets> {
    let jar = find_jars().into_iter().find(|j| j.version == version)?;
    match Assets::open(&jar.path) {
        Ok(a) => Some(a),
        Err(e) => {
            eprintln!("skipping: {version} is not readable: {e:#}");
            None
        }
    }
}

macro_rules! jar {
    ($v:expr) => {
        match open($v) {
            Some(a) => a,
            None => {
                eprintln!("skipping: Minecraft {} is not installed here", $v);
                return;
            }
        }
    };
}

fn block(a: &Assets, name: &str, state: &str) -> mc_unpack::assets::BakedModel {
    a.bake_block(name, &State::parse(state).unwrap()).unwrap()
}

fn close(a: [f32; 3], b: [f32; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
}

#[test]
fn every_version_is_recognised() {
    for (v, layout, support) in [
        ("1.2.5", Layout::Atlas, Support::Unsupported),
        ("1.7.10", Layout::LooseTextures, Support::TexturesOnly),
        ("1.12.2", Layout::LegacyModels, Support::Full),
        ("1.19.2", Layout::Models, Support::Full),
        ("1.20.1", Layout::Models, Support::Full),
        ("1.21.1", Layout::Models, Support::Full),
        ("26.2", Layout::ModelsWithItemDefs, Support::Full),
        ("26.3", Layout::ModelsWithItemDefs, Support::Full),
    ] {
        let a = jar!(v);
        assert_eq!(a.version().id, v);
        assert_eq!(a.layout(), layout, "{v}");
        assert_eq!(a.version().support(), support, "{v}");
    }
}

#[test]
fn the_version_comes_from_version_json_when_there_is_one() {
    use mc_unpack::version::IdSource;
    let a = jar!("26.3");
    assert_eq!(a.version().id_from, IdSource::VersionJson);
    assert!(a.version().json.as_ref().unwrap().world_version.is_some());
    // 1.12.2 predates version.json: the path names it
    let a = jar!("1.12.2");
    assert_eq!(a.version().id_from, IdSource::Path);
}

#[test]
fn unsupported_and_textures_only_jars_say_so() {
    let a = jar!("1.2.5");
    assert!(a.require_models().is_err());
    assert!(check::run(&a).is_err());
    let a = jar!("1.7.10");
    let e = a
        .bake_block("stone", &State::default())
        .unwrap_err()
        .to_string();
    assert!(e.contains("textures only"), "{e}");
    // the textures themselves are readable, and so are the colormaps
    assert!(a.texture("blocks/stone").is_ok());
    assert!(a.colormap(ColormapKind::Grass).is_ok());
    let r = check::run(&a).unwrap();
    assert_eq!(r.total_failed(), 0, "{:?}", r.failures);
    assert!(r.textures.ok > 500);
}

#[test]
fn check_has_no_failures() {
    for v in ["1.12.2", "1.19.2", "1.20.1", "1.21.1", "26.2", "26.3"] {
        let a = jar!(v);
        let r = check::run(&a).unwrap();
        assert_eq!(r.total_failed(), 0, "{v}: {:?}", r.failures);
        assert!(r.blocks.ok > 400, "{v}");
        assert!(r.items.ok > 500, "{v}");
    }
}

#[test]
fn block_entities_are_left_out_not_reconstructed() {
    let a = jar!("26.3");
    let chest = a.bake_block("chest", &State::default()).unwrap();
    assert!(chest.is_empty());
    assert_eq!(
        chest.report.left_out,
        Some(LeftOut::SpecialItem(vec!["chest".into()]))
    );
    // there is no entity model data in the jar to read
    assert!(!a
        .jar()
        .names()
        .iter()
        .any(|n| n.starts_with("assets/minecraft/models/entity")));
    let a = jar!("1.20.1");
    assert_eq!(
        a.classify_item("chest").unwrap(),
        Some(LeftOut::BuiltinEntity)
    );
    assert_eq!(
        a.classify_block("chest").unwrap(),
        Some(LeftOut::BuiltinEntity)
    );
    let a = jar!("1.12.2");
    assert_eq!(
        a.classify_item("chest").unwrap(),
        Some(LeftOut::BuiltinEntity)
    );
    // 1.8 to 1.12 give block entities no blockstate at all
    assert!(a.blockstate("chest").is_err());
}

#[test]
fn grass_block_in_every_model_version() {
    for (v, name, top, state) in [
        ("1.12.2", "grass", "blocks/grass_top", "snowy=false"),
        (
            "1.20.1",
            "grass_block",
            "block/grass_block_top",
            "snowy=false",
        ),
        (
            "26.3",
            "grass_block",
            "block/grass_block_top",
            "snowy=false",
        ),
    ] {
        let a = jar!(v);
        let b = block(&a, name, state);
        // the base cube and the tinted side overlay: 6 + 4 faces
        assert_eq!(b.quad_count(), 10, "{v}");
        let up = b.quads().find(|q| q.texture == top).unwrap();
        assert_eq!(up.tint_index, Some(0));
        assert_eq!(up.normal, [0.0, 1.0, 0.0]);
        let overlay: Vec<&Quad> = b
            .quads()
            .filter(|q| q.texture.ends_with("side_overlay"))
            .collect();
        assert_eq!(overlay.len(), 4);
        assert!(overlay.iter().all(|q| q.tint_index == Some(0)));
        let bounds = |f: fn(&[f32; 3]) -> f32| {
            let all: Vec<f32> = b.quads().flat_map(|q| q.pos.iter().map(f)).collect();
            (
                all.iter().cloned().fold(9.0, f32::min),
                all.iter().cloned().fold(-9.0, f32::max),
            )
        };
        assert_eq!(bounds(|p| p[1]), (0.0, 1.0), "{v}");
    }
}

#[test]
fn the_grass_tint_comes_from_the_jars_own_colormap_and_biome() {
    let a = jar!("26.3");
    let (t, _) = a.tint_for_biome(ColormapKind::Grass, "plains").unwrap();
    assert_eq!(
        t,
        Tint::Colormap {
            kind: ColormapKind::Grass,
            temperature: 0.8,
            downfall: 0.4
        }
    );
    // a lookup that has been compared with the game: plains grass is 0x91bd59
    assert_eq!(a.tint_rgb(&t).unwrap(), [0x91, 0xbd, 0x59]);
    let (f, _) = a.tint_for_biome(ColormapKind::Foliage, "plains").unwrap();
    assert_eq!(a.tint_rgb(&f).unwrap(), [0x77, 0xab, 0x2f]);
    // the biome file's own colour overrides the colormap, and its modifier is reported, not applied
    let (s, notes) = a.tint_for_biome(ColormapKind::Foliage, "swamp").unwrap();
    assert_eq!(s, Tint::Rgb([0x6a, 0x70, 0x39]));
    assert!(notes.is_empty());
    let (_, notes) = a.tint_for_biome(ColormapKind::Grass, "swamp").unwrap();
    assert!(notes.iter().any(|n| n.contains("grass_color_modifier")));
    // 1.12.2 has the same colormap and no biome data
    let old = jar!("1.12.2");
    assert_eq!(
        old.colormap(ColormapKind::Grass).unwrap().lookup(0.8, 0.4),
        [0x91, 0xbd, 0x59]
    );
    assert!(old.biome("plains").is_err());
}

#[test]
fn item_definitions_carry_their_own_tints() {
    let a = jar!("26.3");
    let leaves = a.bake_item("oak_leaves").unwrap();
    assert_eq!(
        leaves.parts[0].tints.get(0),
        Some(&Tint::Rgb([0x48, 0xb5, 0x18]))
    );
    let grass = a.bake_item("grass_block").unwrap();
    assert_eq!(
        grass.parts[0].tints.get(0),
        Some(&Tint::Colormap {
            kind: ColormapKind::Grass,
            temperature: 0.5,
            downfall: 1.0
        })
    );
}

#[test]
fn stairs_facing_east_have_their_high_side_to_the_east() {
    for (v, state) in [
        ("1.12.2", "facing=east,half=bottom,shape=straight"),
        ("1.21.1", "facing=east,half=bottom,shape=straight"),
        ("26.3", "facing=east,half=bottom,shape=straight"),
    ] {
        let a = jar!(v);
        let b = block(&a, "oak_stairs", state);
        // the top of the tall part is at y = 1 and every point there has x > 0.5
        let high: Vec<[f32; 3]> = b
            .quads()
            .flat_map(|q| q.pos.iter().cloned())
            .filter(|p| p[1] == 1.0)
            .collect();
        assert!(!high.is_empty(), "{v}");
        assert!(high.iter().all(|p| p[0] >= 0.5 - 1e-6), "{v}: {high:?}");
        // and a west-facing one has it to the west
        let w = block(&a, "oak_stairs", "facing=west,half=bottom,shape=straight");
        assert!(w
            .quads()
            .flat_map(|q| q.pos.iter().cloned())
            .filter(|p| p[1] == 1.0)
            .all(|p| p[0] <= 0.5 + 1e-6));
    }
}

#[test]
fn uvlock_keeps_planks_aligned_whichever_way_the_stairs_face() {
    let a = jar!("26.3");
    // top faces at y = 1 of all four facings: with uvlock the UV is a function of the world
    // position only, so the same position gets the same UV in every facing
    let mut seen: std::collections::HashMap<(i32, i32), (f32, f32)> = Default::default();
    for f in ["north", "east", "south", "west"] {
        let b = block(
            &a,
            "oak_stairs",
            &format!("facing={f},half=bottom,shape=straight"),
        );
        for q in b
            .quads()
            .filter(|q| q.normal == [0.0, 1.0, 0.0] && q.pos[0][1] == 1.0)
        {
            for i in 0..4 {
                let k = (
                    (q.pos[i][0] * 16.0).round() as i32,
                    (q.pos[i][2] * 16.0).round() as i32,
                );
                let uv = (q.uv[i][0], q.uv[i][1]);
                if let Some(prev) = seen.insert(k, uv) {
                    assert!(
                        (prev.0 - uv.0).abs() < 1e-5 && (prev.1 - uv.1).abs() < 1e-5,
                        "facing {f}: position {k:?} has uv {uv:?}, another facing gave {prev:?}"
                    );
                }
            }
        }
    }
    assert!(seen.len() > 4);
}

#[test]
fn wall_torch_leans_away_from_its_wall() {
    // This pins the sign of element rotation: the torch on a west wall (facing=east) has its
    // top further east than its base
    let a = jar!("26.3");
    let b = block(&a, "redstone_wall_torch", "facing=east,lit=true");
    let pts: Vec<[f32; 3]> = b.quads().flat_map(|q| q.pos.iter().cloned()).collect();
    let mean = |sel: &dyn Fn(&[f32; 3]) -> bool| {
        let v: Vec<f32> = pts.iter().filter(|p| sel(p)).map(|p| p[0]).collect();
        v.iter().sum::<f32>() / v.len() as f32
    };
    assert!(mean(&|p| p[1] > 0.7) > mean(&|p| p[1] < 0.3) + 0.05);
    // it is on the west half of the cell
    assert!(pts.iter().all(|p| p[0] < 0.5));
}

#[test]
fn piston_faces_the_way_its_state_says() {
    let a = jar!("26.3");
    // extended: the base is 12/16 deep, its open face toward the facing
    for (f, axis, sign) in [
        ("east", 0usize, 1.0f32),
        ("west", 0, -1.0),
        ("south", 2, 1.0),
        ("north", 2, -1.0),
        ("up", 1, 1.0),
        ("down", 1, -1.0),
    ] {
        let b = block(&a, "piston", &format!("extended=true,facing={f}"));
        let coords: Vec<f32> = b
            .quads()
            .flat_map(|q| q.pos.iter().map(|p| p[axis]))
            .collect();
        let (lo, hi) = (
            coords.iter().cloned().fold(9.0, f32::min),
            coords.iter().cloned().fold(-9.0, f32::max),
        );
        if sign > 0.0 {
            assert!(
                (lo - 0.0).abs() < 1e-5 && (hi - 0.75).abs() < 1e-5,
                "{f}: {lo} {hi}"
            );
        } else {
            assert!(
                (lo - 0.25).abs() < 1e-5 && (hi - 1.0).abs() < 1e-5,
                "{f}: {lo} {hi}"
            );
        }
    }
}

#[test]
fn multipart_fence_has_arms_where_asked() {
    let a = jar!("26.3");
    let post = block(
        &a,
        "oak_fence",
        "north=false,east=false,south=false,west=false",
    );
    let arms = block(
        &a,
        "oak_fence",
        "north=true,east=true,south=false,west=false",
    );
    assert!(arms.quad_count() > post.quad_count());
    let zs: Vec<f32> = arms
        .quads()
        .flat_map(|q| q.pos.iter().map(|p| p[2]))
        .collect();
    let xs: Vec<f32> = arms
        .quads()
        .flat_map(|q| q.pos.iter().map(|p| p[0]))
        .collect();
    // the north arm reaches z = 0 (north is -Z), the east arm x = 1, nothing reaches the south or west
    assert!(zs.iter().cloned().fold(9.0, f32::min) < 0.01);
    assert!(xs.iter().cloned().fold(-9.0, f32::max) > 0.99);
    assert!(zs.iter().cloned().fold(-9.0, f32::max) < 0.99);
    assert!(xs.iter().cloned().fold(9.0, f32::min) > 0.01);
}

#[test]
fn redstone_wire_with_no_state_uses_the_first_state_its_conditions_describe() {
    let a = jar!("26.3");
    let b = a.bake_block("redstone_wire", &State::default()).unwrap();
    assert!(!b.is_empty());
    assert!(b.report.notes.iter().any(|n| n.contains("first state")));
    // the line texture is drawn translucent because the data says force_translucent
    assert!(b.quads().any(|q| q.translucent));
    // flat: everything sits 1/4 of a pixel above the floor
    assert!(b
        .quads()
        .all(|q| q.pos.iter().all(|p| (p[1] - 0.015625).abs() < 1e-6)));
}

#[test]
fn animated_textures_expose_their_frames() {
    let a = jar!("26.3");
    let t = a.texture("block/water_still").unwrap();
    let an = t.animation.as_ref().unwrap();
    assert!(t.sheet_frames() > 1);
    assert_eq!(t.default_frame().width, t.frame_width);
    assert_eq!(t.default_frame().height, t.frame_height);
    assert!(!an.frames.is_empty());
    assert!(an.frames.iter().all(|f| f.index < t.sheet_frames()));
    // a plain texture has one frame
    let s = a.texture("block/stone").unwrap();
    assert!(s.animation.is_none());
    assert_eq!(s.sheet_frames(), 1);
    // and the sheet of 1.12.2's water is cut the same way
    let old = jar!("1.12.2");
    let w = old.texture("blocks/water_still").unwrap();
    assert!(w.animation.is_some() || w.sheet.height > w.sheet.width);
}

#[test]
fn armor_trim_item_layers_are_generated_from_the_atlas_definition() {
    for v in ["1.20.1", "1.21.1"] {
        let a = jar!(v);
        let t = a.texture("trims/items/boots_trim_amethyst").unwrap();
        let base = a.texture("trims/items/boots_trim").unwrap();
        assert_eq!(
            (t.sheet.width, t.sheet.height),
            (base.sheet.width, base.sheet.height)
        );
        // same shape, different colours
        let same_alpha = t
            .sheet
            .data
            .chunks_exact(4)
            .zip(base.sheet.data.chunks_exact(4))
            .all(|(x, y)| x[3] == y[3]);
        assert!(same_alpha);
        assert_ne!(t.sheet.data, base.sheet.data);
        assert!(a.bake_item("diamond_boots_amethyst_trim").is_ok());
    }
}

#[test]
fn generated_items_are_one_pixel_thick_slabs() {
    let a = jar!("26.3");
    let i = a.bake_item("diamond_sword").unwrap();
    let zs: Vec<f32> = i.quads().flat_map(|q| q.pos.iter().map(|p| p[2])).collect();
    let (lo, hi) = (
        zs.iter().cloned().fold(9.0, f32::min),
        zs.iter().cloned().fold(-9.0, f32::max),
    );
    assert!((lo - 7.5 / 16.0).abs() < 1e-6 && (hi - 8.5 / 16.0).abs() < 1e-6);
    // one front and one back face, and edge strips where the sword meets transparency
    assert_eq!(i.quads().filter(|q| q.face == Dir::South).count(), 1);
    assert_eq!(i.quads().filter(|q| q.face == Dir::North).count(), 1);
    assert!(i.quad_count() > 20);
    // the front face is seen from the south: its top-left corner is at x = 0, y = 1
    let front = i.quads().find(|q| q.face == Dir::South).unwrap();
    assert!(
        close(front.pos[0], [0.0, 1.0, 8.5 / 16.0]),
        "{:?}",
        front.pos
    );
}

#[test]
fn heavy_core_names_a_texture_that_does_not_exist_and_is_left_out() {
    // Mojang's model writes `all` where `#all` was meant, in every face
    let a = jar!("26.3");
    let b = a.bake_block("heavy_core", &State::default()).unwrap();
    assert!(b.is_empty());
    assert_eq!(b.report.left_out, Some(LeftOut::MissingTexture));
}

#[test]
fn provider_loads_everything_it_lists() {
    #[cfg(feature = "bridge")]
    {
        use mc_unpack::bridge::Minecraft;
        use rewrite_bridge::Provider;
        for v in ["1.12.2", "26.3"] {
            let a = jar!(v);
            let mc = Minecraft::from_assets(a).unwrap();
            let list = mc.list("").unwrap();
            assert!(list.len() > 1000, "{v}: {}", list.len());
            for info in list {
                mc.load(&info.id)
                    .unwrap_or_else(|e| panic!("{v} {}: {e}", info.id));
            }
        }
    }
}
