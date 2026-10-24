//! [`rewrite_bridge::Provider`] for Minecraft: Java Edition, read from a client jar.
//!
//! # Ids
//!
//! | id | what |
//! |----|------|
//! | `block/<name>` | the block's blockstate, in the default state (see below) |
//! | `block/<name>?variant=<state>` | the same in a state: `property=value` pairs separated by commas, such as `block/oak_stairs?variant=facing=east,half=bottom,shape=straight` |
//! | `item/<name>` | an item's model: a flat item built from its texture, or the block model it uses |
//!
//! `<name>` is the file name in the jar: `assets/minecraft/blockstates/<name>.json`, and for items
//! `models/item/<name>.json` (1.8 to 1.21.3) or `items/<name>.json` (1.21.4 and later).
//!
//! The default state is the blockstate file's `normal` or empty entry, else its first entry (the
//! game's own default state is in code); for a multipart block that needs properties, the first
//! state its own conditions describe. A property the file does not mention is ignored.
//!
//! # Space
//!
//! 1 block = 1 m, +Y up, right-handed, counter-clockwise front faces, sRGB textures. **Axes are the
//! game's, unchanged**: +X east, +Y up, +Z south, so a position maps to itself (the same mapping
//! minecraft-link uses). A block has no front of its own: which side a furnace shows is the
//! `facing` of its state (`facing=south` puts the face on +Z, the bridge's front). The model's
//! origin is the bottom centre of the block's cell, as for hytale-unpack's blocks, so a full cube
//! spans x and z from -0.5 to 0.5 and y from 0 to 1.
//!
//! # Materials
//!
//! One material per texture and tint: the texture's frame 0 (or the frame configured), matte.
//! Filtering is nearest in the glTF files this crate writes; the bridge's `Material` has no sampler
//! field, so a renderer has to choose nearest itself. Alpha: `Opaque` when every texel is opaque,
//! `Mask(0.5)` when texels are opaque or fully transparent, `Blend` otherwise or when the data says
//! `force_translucent`. The game's real render layer is in code (before 26.x entirely), so this is
//! what the pixels permit, not a statement about the game.
//!
//! # Tints
//!
//! A face with a `tintindex` is multiplied by a colour the game picks in code. The provider leaves
//! those faces white unless the data says otherwise (26.x item definitions) or the caller supplies
//! a [`TintSet`] with [`Minecraft::with_tints`].

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{anyhow, bail, Result};
use rewrite_bridge::{
    AlphaMode, AssetInfo, AssetKind, BoxError, Image, Material, Mesh, Model, Node, Provider,
};

pub use crate::assets::AssetId;
use crate::assets::{Assets, BakedModel, LeftOut};
use crate::install::{find_jars, JarFile};
use crate::mesh::{self, MeshOptions, Origin};
use crate::texture::AlphaClass;
use crate::tint::TintSet;
use crate::version::Support;

/// The game's own axes carry over, so the bridge's up is the game's up
pub const GAME: &str = "Minecraft";

/// What a load found and left out, besides the model
#[derive(Clone, Debug)]
pub struct LoadReport {
    pub baked: BakedModel,
    pub quads: usize,
    pub materials: usize,
}

/// A block or item whose data has no geometry
#[derive(Debug)]
pub struct LeftOutError {
    pub id: String,
    pub why: LeftOut,
}

impl std::fmt::Display for LeftOutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: left out: {}", self.id, self.why)
    }
}

impl std::error::Error for LeftOutError {}

pub struct Minecraft {
    assets: Assets,
    tints: TintSet,
    frame: u32,
    listing: OnceLock<Vec<AssetInfo>>,
}

fn berr(e: anyhow::Error) -> BoxError {
    format!("{e:#}").into()
}

impl Minecraft {
    /// Opens a client jar. Fails for jars without JSON models (before 1.8): those have textures
    /// only, readable through [`Assets`].
    pub fn open(path: &Path) -> Result<Minecraft> {
        Self::from_assets(Assets::open(path)?)
    }

    pub fn from_assets(assets: Assets) -> Result<Minecraft> {
        assets.require_models()?;
        Ok(Minecraft {
            assets,
            tints: TintSet::new(),
            frame: 0,
            listing: OnceLock::new(),
        })
    }

    /// Opens the newest installed version this crate can read (`MC_UNPACK_JAR` first when set)
    pub fn open_default() -> Result<Minecraft> {
        let jars = find_jars();
        let mut best: Option<(Assets, &JarFile)> = None;
        for j in jars.iter().rev() {
            if let Ok(a) = Assets::open(&j.path) {
                if a.version().support() == Support::Full {
                    best = Some((a, j));
                    break;
                }
            }
        }
        match best {
            Some((a, _)) => Self::from_assets(a),
            None => bail!(
                "no readable Minecraft client jar found (looked for Prism Launcher and vanilla launcher installs, and MC_UNPACK_JAR)"
            ),
        }
    }

    /// Colours for tint indices; every model loaded afterwards is multiplied by them
    pub fn with_tints(mut self, tints: TintSet) -> Self {
        self.tints = tints;
        self
    }

    /// Which frame of an animated texture the materials show (0 by default)
    pub fn with_frame(mut self, frame: u32) -> Self {
        self.frame = frame;
        self
    }

    pub fn assets(&self) -> &Assets {
        &self.assets
    }

    fn mesh_options(&self) -> MeshOptions {
        MeshOptions {
            frame: self.frame,
            tints: self.tints.clone(),
            origin: Origin::BottomCentre,
        }
    }

    /// Bakes the block or item an id names
    pub fn bake(&self, id: &str) -> Result<BakedModel> {
        self.assets.bake_id(id)
    }

    /// Loads with a report of what was baked
    pub fn load_report(&self, id: &str) -> Result<(Model, LoadReport)> {
        let baked = self.bake(id)?;
        if baked.is_empty() {
            return Err(LeftOutError {
                id: id.to_string(),
                why: baked.report.left_out.clone().unwrap_or(LeftOut::NoElements),
            }
            .into());
        }
        let groups = mesh::build(&self.assets, &baked, &self.mesh_options())?;
        let mut model = Model {
            name: id.to_string(),
            nodes: vec![Node::new(id, None)],
            ..Model::default()
        };
        for g in &groups {
            let mut positions = Vec::with_capacity(g.quads.len() * 4);
            let mut normals = Vec::with_capacity(g.quads.len() * 4);
            let mut uvs = Vec::with_capacity(g.quads.len() * 4);
            let mut indices = Vec::with_capacity(g.quads.len() * 6);
            for q in &g.quads {
                let base = positions.len() as u32;
                for i in 0..4 {
                    positions.push(q.pos[i]);
                    normals.push(q.normal);
                    uvs.push(q.uv[i]);
                }
                indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
            }
            let name = match g.tint_index {
                Some(t) => format!("{}#tint{t}", g.texture),
                None => g.texture.clone(),
            };
            model.materials.push(Material {
                name: name.clone(),
                base_color: Some(Image {
                    width: g.image.width,
                    height: g.image.height,
                    rgba: g.image.data.clone(),
                }),
                base_color_factor: [1.0; 4],
                metallic: 0.0,
                roughness: 1.0,
                alpha: match g.alpha {
                    AlphaClass::Opaque => AlphaMode::Opaque,
                    AlphaClass::Mask => AlphaMode::Mask(0.5),
                    AlphaClass::Blend => AlphaMode::Blend,
                },
                double_sided: false,
                ..Material::default()
            });
            model.meshes.push(Mesh {
                name,
                node: 0,
                material: model.materials.len() - 1,
                positions,
                normals,
                uvs,
                indices,
                ..Mesh::default()
            });
        }
        model
            .validate()
            .map_err(|e| anyhow!("{id}: the model built is invalid: {e}"))?;
        let report = LoadReport {
            quads: baked.quad_count(),
            materials: model.materials.len(),
            baked,
        };
        Ok((model, report))
    }

    fn build_listing(&self) -> Vec<AssetInfo> {
        let mut out = Vec::new();
        for n in self.assets.block_names() {
            if matches!(self.assets.classify_block(&n), Ok(None)) {
                out.push(AssetInfo {
                    id: format!("block/{n}"),
                    name: n,
                    kind: AssetKind::Prop,
                });
            }
        }
        for n in self.assets.item_names() {
            if matches!(self.assets.classify_item(&n), Ok(None)) {
                out.push(AssetInfo {
                    id: format!("item/{n}"),
                    name: n,
                    kind: AssetKind::Prop,
                });
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Everything that draws something, unfiltered
    pub fn listing(&self) -> &[AssetInfo] {
        self.listing.get_or_init(|| self.build_listing())
    }

    /// The state keys a block's blockstate file lists (for `variants` files), or its properties and
    /// values (for multipart), as text lines for the CLI
    pub fn describe_states(&self, name: &str) -> Result<Vec<String>> {
        let bs = self.assets.blockstate(name)?;
        let mut out = Vec::new();
        if !bs.variants.is_empty() {
            for k in bs.keys() {
                out.push(if k.is_empty() {
                    "(no properties)".into()
                } else {
                    k.to_string()
                });
            }
        } else {
            let pv: BTreeMap<_, _> = bs.property_values();
            for (p, vs) in pv {
                out.push(format!(
                    "{p}: {}",
                    vs.into_iter().collect::<Vec<_>>().join(" | ")
                ));
            }
        }
        Ok(out)
    }
}

impl Provider for Minecraft {
    fn game(&self) -> &str {
        GAME
    }

    fn list(&self, filter: &str) -> Result<Vec<AssetInfo>, BoxError> {
        let f = filter.to_ascii_lowercase();
        Ok(self
            .listing()
            .iter()
            .filter(|a| {
                f.is_empty()
                    || a.id.to_ascii_lowercase().contains(&f)
                    || a.name.to_ascii_lowercase().contains(&f)
            })
            .cloned()
            .collect())
    }

    fn load(&self, id: &str) -> Result<Model, BoxError> {
        self.load_report(id).map(|r| r.0).map_err(|e| {
            // keep the typed error of a left-out asset reachable
            match e.downcast::<LeftOutError>() {
                Ok(l) => Box::new(l) as BoxError,
                Err(e) => berr(e),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::tests::tiny_jar;

    fn mc() -> Minecraft {
        Minecraft::from_assets(Assets::from_jar(tiny_jar()).unwrap()).unwrap()
    }

    #[test]
    fn ids() {
        assert_eq!(
            AssetId::parse("block/oak_stairs?variant=facing=east,half=bottom").unwrap(),
            AssetId::Block {
                name: "oak_stairs".into(),
                variant: Some("facing=east,half=bottom".into())
            }
        );
        assert_eq!(
            AssetId::parse("item/stick").unwrap(),
            AssetId::Item {
                name: "stick".into()
            }
        );
        assert!(AssetId::parse("stick").is_err());
        assert!(AssetId::parse("item/stick?variant=a=b").is_err());
        assert!(AssetId::parse("block/x?foo=1").is_err());
    }

    #[test]
    fn a_cube_is_one_metre_on_its_bottom_centre() {
        let m = mc();
        let model = m.load("block/cube").unwrap();
        let (lo, hi) = model.bounds().unwrap();
        assert_eq!(lo, [-0.5, 0.0, -0.5]);
        assert_eq!(hi, [0.5, 1.0, 0.5]);
        assert_eq!(model.meshes.len(), 1);
        assert_eq!(model.materials[0].alpha, AlphaMode::Opaque);
        model.validate().unwrap();
    }

    #[test]
    fn variants_change_the_geometry() {
        let m = mc();
        let n = m.load("block/step?variant=facing=north").unwrap();
        let e = m.load("block/step?variant=facing=east").unwrap();
        // the north face of the north variant is at z = -0.5, of the east one at x = 0.5
        let nz = n.meshes[0]
            .positions
            .iter()
            .map(|p| p[2])
            .fold(9.0f32, f32::min);
        assert_eq!(nz, -0.5);
        let ex = e.meshes[0]
            .positions
            .iter()
            .map(|p| p[0])
            .fold(-9.0f32, f32::max);
        assert_eq!(ex, 0.5);
    }

    #[test]
    fn cutout_texture_is_masked_and_listing_skips_what_has_no_geometry() {
        let m = mc();
        let leaves = m.load("block/leaves").unwrap();
        assert_eq!(leaves.materials[0].alpha, AlphaMode::Mask(0.5));
        let ids: Vec<String> = m.list("").unwrap().into_iter().map(|a| a.id).collect();
        assert!(ids.contains(&"block/cube".to_string()));
        assert!(ids.contains(&"item/leaf".to_string()));
        assert!(!ids.contains(&"block/chest".to_string()));
        assert!(!ids.contains(&"item/generated".to_string()));
        assert!(!ids.contains(&"item/entity".to_string()));
        assert_eq!(m.list("LEA").unwrap().len(), 2);
    }

    #[test]
    fn a_left_out_block_is_a_typed_error() {
        let m = mc();
        let e = m.load("block/chest").unwrap_err();
        let l = e.downcast_ref::<LeftOutError>().expect("typed");
        assert_eq!(l.why, LeftOut::NoElements);
        assert!(m.load("block/nothing").is_err());
    }

    #[test]
    fn provider_names_the_game() {
        assert_eq!(mc().game(), "Minecraft");
    }

    #[test]
    fn tints_set_on_the_provider_are_multiplied_in() {
        use crate::tint::Tint;
        let m = mc().with_tints(TintSet::new().with(0, Tint::Rgb([0, 255, 255])));
        let model = m.load("block/leaves").unwrap();
        // the green texel (0,200,0) keeps G and loses nothing it had; red stays 0
        let img = model.materials[0].base_color.as_ref().unwrap();
        assert_eq!(&img.rgba[0..4], &[0, 200, 0, 255]);
        let plain = mc().load("block/leaves").unwrap();
        assert_eq!(plain.materials[0].name, "block/leaf#tint0");
    }
}
