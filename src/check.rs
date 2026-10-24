//! The sweep behind `mc-unpack check`: parse and bake everything a jar has, count what fails.
//!
//! What is counted, for a jar with JSON models:
//!
//! | what | ok means |
//! |------|----------|
//! | blockstates | the file parses (variants or multipart, quarter-turn rotations ...) |
//! | block models, item models | the file parses and its parent chain resolves to a root |
//! | item definitions (1.21.4+) | the file is JSON and its default branch can be walked |
//! | blocks | every model the blockstate mentions bakes, and every texture its faces use decodes |
//! | items | the default models bake (a flat item's texture decodes) |
//! | textures | every PNG under `textures/` decodes, every `.mcmeta` parses and fits its sheet |
//! | colormaps | `grass.png`, `foliage.png` (and `dry_foliage.png`) are 256 x 256 |
//!
//! Blocks and items whose data has no geometry (block entities, `special` items, invisible blocks)
//! are **left out, not failed**, and counted apart.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use anyhow::{bail, Result};

use crate::assets::{Assets, LeftOut};
use crate::bake::{bake_model, VariantRotation};
use crate::blockstate::BlockState;
use crate::model::{Builtin, RawModel};
use crate::texture::{decode_png, McMeta, Texture};
use crate::tint::ColormapKind;
use crate::version::{Layout, Support};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Count {
    pub ok: u64,
    pub failed: u64,
}

impl Count {
    fn tally(&mut self, ok: bool) {
        if ok {
            self.ok += 1
        } else {
            self.failed += 1
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub version: String,
    pub layout: Option<Layout>,
    pub support: Option<Support>,
    pub blockstates: Count,
    pub block_models: Count,
    pub item_models: Count,
    pub item_defs: Count,
    pub blocks: Count,
    pub items: Count,
    pub textures: Count,
    pub mcmetas: Count,
    pub animated_textures: u64,
    pub colormaps: Count,
    /// Blocks and items with no geometry in the data, by reason
    pub blocks_left_out: BTreeMap<String, Vec<String>>,
    pub items_left_out: BTreeMap<String, Vec<String>>,
    /// Item models that are templates (`item/generated`, `item/handheld` ...), not items
    pub item_templates: u64,
    pub quads: u64,
    pub failures: Vec<String>,
    /// Things in Mojang's files that are wrong and that the game tolerates; not counted as failures
    pub upstream_defects: Vec<String>,
    /// Faces and elements left out inside models that otherwise bake (a texture variable nothing
    /// defines, a rotation whose order the data does not give): each is a line, not a failure
    pub left_out_inside_models: Vec<String>,
    /// Items that lost a part to code or to a refusal (a `special` layer of a composite, say)
    pub items_partial: u64,
    /// The biggest things met, to be read against the safety limits (see
    /// `texture::MAX_PIXELS`, `texture::MAX_SHEET_FRAMES`, `texture::MAX_FRAME_LIST`,
    /// `item::MAX_LAYER_ELEMENTS`)
    pub max_png_pixels: u64,
    pub max_sheet_frames: u32,
    pub max_frame_list: usize,
    pub max_item_quads: usize,
    pub seconds: f64,
}

const MAX_LISTED: usize = 60;

impl Report {
    fn fail(&mut self, what: &str, name: &str, e: impl std::fmt::Display) {
        if self.failures.len() < MAX_LISTED {
            self.failures.push(format!("{what} {name}: {e:#}"));
        }
    }

    pub fn total_failed(&self) -> u64 {
        self.blockstates.failed
            + self.block_models.failed
            + self.item_models.failed
            + self.item_defs.failed
            + self.blocks.failed
            + self.items.failed
            + self.textures.failed
            + self.mcmetas.failed
            + self.colormaps.failed
    }
}

fn left_out_key(l: &LeftOut) -> String {
    match l {
        LeftOut::BuiltinEntity => "builtin/entity model".into(),
        LeftOut::SpecialItem(k) => format!("special item model: {}", k.join(", ")),
        LeftOut::NoElements => "model without elements".into(),
        LeftOut::NoPartMatched => "no multipart part matched".into(),
        LeftOut::Template => "template".into(),
        LeftOut::Refused(why) => format!("refused: {why}"),
        LeftOut::MissingTexture => {
            "every face names a texture that does not exist (upstream defect)".into()
        }
    }
}

pub fn run(a: &Assets) -> Result<Report> {
    let start = Instant::now();
    let mut r = Report {
        version: a.version().id.clone(),
        layout: Some(a.layout()),
        support: Some(a.version().support()),
        ..Report::default()
    };
    if a.layout() == Layout::Atlas {
        bail!(
            "Minecraft {}: unsupported ({})",
            a.version().id,
            a.version().support_note()
        );
    }
    check_textures(a, &mut r);
    if a.layout().has_json_models() {
        check_models(a, &mut r);
    }
    r.seconds = start.elapsed().as_secs_f64();
    Ok(r)
}

fn check_textures(a: &Assets, r: &mut Report) {
    let jar = a.jar();
    let prefix = "assets/minecraft/textures/";
    let names: Vec<String> = jar
        .names_under(prefix)
        .filter(|n| n.ends_with(".png") || n.ends_with(".png.mcmeta"))
        .map(String::from)
        .collect();
    for n in &names {
        let short = n.strip_prefix(prefix).unwrap_or(n);
        if let Some(png) = n.strip_suffix(".mcmeta") {
            let res = (|| -> Result<Option<(u32, usize)>> {
                let meta = McMeta::parse(jar.read_json(n)?)?;
                if meta.animation.is_some() && jar.contains(png) {
                    let img = decode_png(&jar.read_png(png)?)?;
                    let t = Texture::new(img, Some(&meta))?;
                    if let Some(an) = &t.animation {
                        return Ok(Some((t.sheet_frames(), an.frames.len())));
                    }
                }
                Ok(None)
            })();
            match res {
                Ok(anim) => {
                    r.mcmetas.ok += 1;
                    if let Some((frames, listed)) = anim {
                        r.animated_textures += 1;
                        r.max_sheet_frames = r.max_sheet_frames.max(frames);
                        r.max_frame_list = r.max_frame_list.max(listed);
                    }
                }
                Err(e) => {
                    r.mcmetas.failed += 1;
                    r.fail("mcmeta", short, e);
                }
            }
        } else {
            let res = jar
                .read_png(n)
                .and_then(|b| decode_png(&b).map(|i| i.width as u64 * i.height as u64));
            match res {
                Ok(px) => {
                    r.textures.ok += 1;
                    r.max_png_pixels = r.max_png_pixels.max(px);
                }
                Err(e) => {
                    r.textures.failed += 1;
                    r.fail("texture", short, e);
                }
            }
        }
    }
    for k in [
        ColormapKind::Grass,
        ColormapKind::Foliage,
        ColormapKind::DryFoliage,
    ] {
        if !jar.contains(k.file()) {
            continue;
        }
        match a.colormap(k) {
            Ok(_) => r.colormaps.ok += 1,
            Err(e) => {
                r.colormaps.failed += 1;
                r.fail("colormap", k.name(), e);
            }
        }
    }
}

fn check_models(a: &Assets, r: &mut Report) {
    // Raw files first: everything must parse
    for n in a.block_names() {
        match a.blockstate(&n) {
            Ok(_) => r.blockstates.ok += 1,
            Err(e) => {
                r.blockstates.failed += 1;
                r.fail("blockstate", &n, e);
            }
        }
    }
    let jar = a.jar();
    for (folder, count) in [("block", 0), ("item", 1)] {
        let prefix = format!("assets/minecraft/models/{folder}/");
        let names: Vec<String> = jar
            .names_under(&prefix)
            .filter(|n| n.ends_with(".json"))
            .map(String::from)
            .collect();
        for n in names {
            let id = n
                .strip_prefix("assets/minecraft/models/")
                .and_then(|s| s.strip_suffix(".json"))
                .unwrap_or(&n)
                .to_string();
            let ok = (|| -> Result<()> {
                let text = jar.read_json(&n)?;
                RawModel::parse(&text)?;
                a.model(&id)?;
                Ok(())
            })();
            let c = if count == 0 {
                &mut r.block_models
            } else {
                &mut r.item_models
            };
            c.tally(ok.is_ok());
            if let Err(e) = ok {
                r.fail("model", &id, e);
            }
        }
    }
    if a.layout() == Layout::ModelsWithItemDefs {
        for n in a.item_names() {
            match a.item_definition(&n) {
                Ok(_) => r.item_defs.ok += 1,
                Err(e) => {
                    r.item_defs.failed += 1;
                    r.fail("item definition", &n, e);
                }
            }
        }
    }
    // Blocks: bake every model every blockstate mentions
    let mut referenced: BTreeSet<String> = BTreeSet::new();
    for n in a.block_names() {
        let Ok(bs) = a.blockstate(&n) else { continue };
        let mut inside = Vec::new();
        let res = bake_all_variants(a, &bs, &mut r.quads, &mut referenced, &mut inside);
        r.left_out_inside_models.extend(inside);
        match res {
            Ok(None) => r.blocks.ok += 1,
            Ok(Some(lo)) => {
                r.blocks.ok += 1;
                // say more when the item of the same name shows the block is drawn in code
                let lo = match (lo, a.classify_block(&n)) {
                    (LeftOut::NoElements, Ok(Some(better))) => better,
                    (lo, _) => lo,
                };
                r.blocks_left_out
                    .entry(left_out_key(&lo))
                    .or_default()
                    .push(n.clone());
            }
            Err(e) => {
                r.blocks.failed += 1;
                r.fail("block", &n, e);
            }
        }
    }
    // Items
    for n in a.item_names() {
        // templates are models other models inherit from: no layers, nothing to draw
        if a.layout() != Layout::ModelsWithItemDefs {
            if let Ok(m) = a.model(&format!("item/{n}")) {
                if m.builtin == Some(Builtin::Generated) && m.layers().is_empty() {
                    r.item_templates += 1;
                    continue;
                }
            }
        }
        match a.bake_item(&n) {
            Ok(b) if !b.report.failed_parts.is_empty() => {
                // one model of the item baked and another failed: the item is only half there
                r.items.failed += 1;
                for f in &b.report.failed_parts {
                    r.fail("item part", &n, f);
                }
            }
            Ok(b) => {
                r.items.ok += 1;
                r.quads += b.quad_count() as u64;
                r.max_item_quads = r.max_item_quads.max(b.quad_count());
                if !b.parts.is_empty() && !b.report.dropped_parts.is_empty() {
                    r.items_partial += 1;
                }
                r.left_out_inside_models.extend(
                    b.report
                        .notes
                        .iter()
                        .filter(|n| {
                            !n.starts_with("a `special`")
                                && (n.contains("was left out") || n.contains("were left out"))
                        })
                        .cloned(),
                );
                for q in b.quads() {
                    referenced.insert(q.texture.clone());
                }
                if let Some(lo) = b.report.left_out {
                    r.items_left_out
                        .entry(left_out_key(&lo))
                        .or_default()
                        .push(n.clone());
                }
            }
            Err(e) => {
                r.items.failed += 1;
                r.fail("item", &n, e);
            }
        }
    }
    // Every texture a face uses has to exist and decode (the pass above only counted PNGs)
    for t in &referenced {
        if let Err(e) = a.texture(t) {
            // A name with no folder (`all` instead of `#all`) cannot be a texture: the game reads
            // it as a missing texture and goes on. That is a defect in Mojang's file, not here.
            if !t.contains('/') {
                r.upstream_defects.push(format!("a face names texture `{t}`, which cannot exist (a `#` is missing?); the game draws its missing-texture pattern"));
            } else {
                r.blocks.failed += 1;
                r.fail("texture of a face", t, e);
            }
        }
    }
}

fn bake_all_variants(
    a: &Assets,
    bs: &BlockState,
    quads: &mut u64,
    referenced: &mut BTreeSet<String>,
    inside: &mut Vec<String>,
) -> Result<Option<LeftOut>> {
    let mut drawn = false;
    let mut first_left_out = None;
    for v in bs.all_variants() {
        let id = a.variant_model_id(&v.model)?;
        let m = a.model(&id)?;
        if let Some(lo) = a.left_out_of(&m) {
            first_left_out.get_or_insert(lo);
            continue;
        }
        let (q, face_notes) = bake_model(
            &m,
            VariantRotation {
                x: v.x,
                y: v.y,
                uvlock: v.uvlock,
            },
        )?;
        inside.extend(face_notes);
        *quads += q.len() as u64;
        for quad in &q {
            referenced.insert(quad.texture.clone());
        }
        drawn |= !q.is_empty();
    }
    if drawn {
        Ok(None)
    } else {
        Ok(Some(first_left_out.unwrap_or(LeftOut::NoElements)))
    }
}

/// Peak resident set size of this process in bytes
pub fn peak_rss() -> u64 {
    #[cfg(unix)]
    {
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) } == 0 {
            let v = ru.ru_maxrss as u64;
            // macOS reports bytes, Linux kilobytes
            return if cfg!(target_os = "macos") {
                v
            } else {
                v * 1024
            };
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::tests::tiny;

    #[test]
    fn sweeps_a_tiny_jar() {
        let a = tiny();
        let r = run(&a).unwrap();
        assert_eq!(r.total_failed(), 0, "{:?}", r.failures);
        assert_eq!(r.blockstates.ok, 5);
        assert!(r.blocks.ok == 5);
        // the chest-like block is left out, not failed
        assert_eq!(
            r.blocks_left_out
                .get("model without elements")
                .map(|v| v.len()),
            Some(1)
        );
        assert_eq!(
            r.items_left_out
                .get("builtin/entity model")
                .map(|v| v.len()),
            Some(1)
        );
        // item/generated is a template
        assert_eq!(r.item_templates, 1);
        assert!(r.textures.ok >= 3);
    }

    #[test]
    fn a_broken_model_is_counted_and_named() {
        use crate::jar::testjar::{png, JarBuilder};
        let jar = JarBuilder::new()
            .add(
                "assets/minecraft/blockstates/a.json",
                r#"{"variants":{"":{"model":"block/missing"}}}"#,
            )
            .add("assets/minecraft/blockstates/b.json", "{not json")
            .add(
                "assets/minecraft/models/block/bad.json",
                r#"{"parent":"block/nowhere"}"#,
            )
            .add(
                "assets/minecraft/textures/block/t.png",
                png(1, 1, |_, _| [0, 0, 0, 255]),
            )
            .build();
        let a = Assets::from_jar(jar).unwrap();
        let r = run(&a).unwrap();
        assert_eq!(r.blockstates.failed, 1);
        assert_eq!(r.block_models.failed, 1);
        // block a: its model is missing
        assert_eq!(r.blocks.failed, 1);
        assert!(
            r.failures.iter().any(|f| f.contains("block a")),
            "{:?}",
            r.failures
        );
    }
}
