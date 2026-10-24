//! [`Assets`]: one client jar, with the caches and the operations that need several layers
//! (a block name to quads, an item name to quads, a texture name to pixels).

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::bake::{bake_element, bake_model, Quad, VariantRotation};
use crate::blockstate::{BlockState, State};
use crate::item::{self, DefaultModels};
use crate::jar::Jar;
use crate::model::{normalize_location, resolve, Builtin, RawModel, ResolvedModel};
use crate::texture::{decode_png, McMeta, Texture};
use crate::tint::{Biome, Colormap, ColormapKind, Tint, TintSet};
use crate::version::{self, Layout, Support, VersionInfo};

const ROOT: &str = "assets/minecraft";

/// Data this crate will not guess about: ambiguous, over a safety limit, or made of a rule that is
/// in the game's code. Reported as [`LeftOut::Refused`] with the reason.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

/// A model that has nothing to draw from data, as an error (so that a caller that wanted a
/// drawing can tell it from a failure)
#[derive(Debug)]
pub struct NotData(pub String, pub LeftOut);

impl std::fmt::Display for NotData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.0, self.1)
    }
}

impl std::error::Error for NotData {}

/// The geometry of one model plus the tints that came with it
#[derive(Clone, Debug)]
pub struct Part {
    /// The model file that made it (`block/oak_stairs`)
    pub model: String,
    pub quads: Vec<Quad>,
    /// Tints the data itself attaches to this part (26.x item definitions); empty otherwise
    pub tints: TintSet,
}

/// Why a block or item has no geometry from the data
#[derive(Clone, Debug, PartialEq)]
pub enum LeftOut {
    /// The model's root is `builtin/entity`: the game draws it in code
    BuiltinEntity,
    /// An item definition names a `special` model (chest, banner, head ...): drawn in code
    SpecialItem(Vec<String>),
    /// The model has no elements: either nothing to see (barrier, air, light) or drawn in code
    /// (block entities and entity-like blocks, whose blockstate model is only a particle texture)
    NoElements,
    /// A multipart blockstate whose parts all depend on properties the state did not give
    NoPartMatched,
    /// Every face names a texture that cannot exist, so the game would draw only its built-in
    /// missing-texture pattern (Mojang's `heavy_core` model writes `all` where `#all` was meant)
    MissingTexture,
    /// Something this crate will not guess about (ambiguous data, a size over its safety limit);
    /// the text says what and why
    Refused(String),
    /// A model other models inherit from (`item/generated`, `item/handheld`): no texture of its own
    Template,
}

impl std::fmt::Display for LeftOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LeftOut::BuiltinEntity => write!(f, "drawn by the game's code (`builtin/entity` model)"),
            LeftOut::SpecialItem(k) => write!(
                f,
                "drawn by the game's code (special item model `{}`)",
                k.join("`, `")
            ),
            LeftOut::NoElements => write!(
                f,
                "the data has no geometry for it (invisible, or drawn by the game's code: a block entity or an entity)"
            ),
            LeftOut::NoPartMatched => write!(f, "no part of the blockstate matches the given state"),
            LeftOut::MissingTexture => write!(
                f,
                "every face names a texture that does not exist; the game draws its built-in missing-texture pattern (code, not data)"
            ),
            LeftOut::Refused(why) => write!(f, "refused: {why}"),
            LeftOut::Template => write!(f, "a template other models inherit from, not an item"),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    /// The `variants` key picked, if any
    pub state_key: Option<String>,
    /// Properties of the requested state that the blockstate does not mention
    pub ignored_properties: Vec<String>,
    /// How many `variants` keys fit a partial state (the first was used)
    pub candidates: usize,
    pub notes: Vec<String>,
    pub left_out: Option<LeftOut>,
    /// Parts of an item that failed to bake (a missing texture in one model of a composite, say)
    /// while others did. Each is also a line in `notes`; `check` counts them as failures.
    pub failed_parts: Vec<String>,
    /// Parts of an item left out (drawn by code, or refused) while others were baked; empty when
    /// the whole item is left out (then `left_out` says why)
    pub dropped_parts: Vec<String>,
    /// The state the model was built from, written as the `?variant=` text that reproduces it:
    /// the blockstate entry used, or the given state, or the one derived for a multipart block
    pub state_used: Option<String>,
}

/// A block or an item, baked
#[derive(Clone, Debug)]
pub struct BakedModel {
    /// `block/grass_block` or `item/diamond_sword`
    pub id: String,
    pub parts: Vec<Part>,
    pub report: Report,
}

impl BakedModel {
    pub fn quads(&self) -> impl Iterator<Item = &Quad> {
        self.parts.iter().flat_map(|p| p.quads.iter())
    }

    pub fn quad_count(&self) -> usize {
        self.parts.iter().map(|p| p.quads.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.quad_count() == 0
    }

    pub fn texture_paths(&self) -> BTreeSet<&str> {
        self.quads().map(|q| q.texture.as_str()).collect()
    }

    pub fn tint_indices(&self) -> BTreeSet<u32> {
        self.quads().filter_map(|q| q.tint_index).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssetId {
    Block {
        name: String,
        variant: Option<String>,
    },
    Item {
        name: String,
    },
}

impl AssetId {
    pub fn parse(id: &str) -> Result<AssetId> {
        let (path, query) = match id.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (id, None),
        };
        let variant = match query {
            None => None,
            Some(q) => match q.strip_prefix("variant=") {
                Some(v) => Some(v.to_string()),
                None => bail!("{id}: the only query is `?variant=<state>`"),
            },
        };
        if let Some(n) = path.strip_prefix("block/") {
            if n.is_empty() {
                bail!("{id}: no block name");
            }
            Ok(AssetId::Block {
                name: n.to_string(),
                variant,
            })
        } else if let Some(n) = path.strip_prefix("item/") {
            if n.is_empty() {
                bail!("{id}: no item name");
            }
            if variant.is_some() {
                bail!("{id}: items have no variants");
            }
            Ok(AssetId::Item {
                name: n.to_string(),
            })
        } else {
            bail!("{id}: ids are `block/<name>[?variant=<state>]` and `item/<name>`")
        }
    }
}

pub struct Assets {
    jar: Jar,
    version: VersionInfo,
    blockstates: Mutex<HashMap<String, Arc<BlockState>>>,
    raw_models: Mutex<HashMap<String, Arc<RawModel>>>,
    resolved: Mutex<HashMap<String, Arc<ResolvedModel>>>,
    textures: Mutex<HashMap<String, Arc<Texture>>>,
    colormaps: Mutex<HashMap<ColormapKind, Arc<Colormap>>>,
    permutations: OnceLock<HashMap<String, PermSource>>,
}

/// A texture the game generates when it builds an atlas (`atlases/*.json`, source type
/// `paletted_permutations`): a base texture whose palette-key colours are swapped for another
/// palette's. The armor trims on items are these.
#[derive(Clone, Debug, PartialEq)]
struct PermSource {
    base: String,
    palette_key: String,
    palette: String,
}

impl Assets {
    pub fn open(path: &Path) -> Result<Assets> {
        let jar = Jar::open(path)?;
        let version = version::detect(&jar, Some(path));
        Self::finish(jar, version, path)
    }

    pub fn from_jar(jar: Jar) -> Result<Assets> {
        let version = version::detect(&jar, jar.path());
        let label = jar
            .path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(memory)".into());
        Self::finish(jar, version, Path::new(&label))
    }

    fn finish(jar: Jar, version: VersionInfo, path: &Path) -> Result<Assets> {
        // 1.2.5 keeps its art on atlas sheets at the jar's root and has no assets folder
        if jar.names_under("assets/minecraft/").next().is_none() && !jar.contains("terrain.png") {
            bail!(
                "{} has no assets/minecraft folder and no terrain.png: it is not a Minecraft client jar (a modded profile's jar, or a server jar?)",
                path.display()
            );
        }
        Ok(Assets {
            jar,
            version,
            blockstates: Mutex::new(HashMap::new()),
            raw_models: Mutex::new(HashMap::new()),
            resolved: Mutex::new(HashMap::new()),
            textures: Mutex::new(HashMap::new()),
            colormaps: Mutex::new(HashMap::new()),
            permutations: OnceLock::new(),
        })
    }

    pub fn jar(&self) -> &Jar {
        &self.jar
    }

    pub fn version(&self) -> &VersionInfo {
        &self.version
    }

    pub fn layout(&self) -> Layout {
        self.version.layout
    }

    /// Fails with the reason when this jar's layout has no JSON models
    pub fn require_models(&self) -> Result<()> {
        if !self.layout().has_json_models() {
            bail!(
                "Minecraft {}: {} ({})",
                self.version.id,
                match self.version.support() {
                    Support::TexturesOnly => "textures only, no models",
                    _ => "unsupported",
                },
                self.version.support_note()
            );
        }
        Ok(())
    }

    // ---- names ----

    /// The names of all blockstate files (`stone`, `oak_stairs` ...)
    pub fn block_names(&self) -> Vec<String> {
        self.names_in("blockstates/", ".json")
    }

    /// The names of the items that have a model: `items/` definitions in 1.21.4 and later, else
    /// the files in `models/item/`
    pub fn item_names(&self) -> Vec<String> {
        if self.layout() == Layout::ModelsWithItemDefs {
            self.names_in("items/", ".json")
        } else {
            self.names_in("models/item/", ".json")
        }
    }

    pub fn texture_names(&self) -> Vec<String> {
        self.names_in("textures/", ".png")
    }

    fn names_in(&self, folder: &str, ext: &str) -> Vec<String> {
        let prefix = format!("{ROOT}/{folder}");
        self.jar
            .names_under(&prefix)
            .filter_map(|n| n.strip_prefix(prefix.as_str()))
            .filter_map(|n| n.strip_suffix(ext))
            .map(String::from)
            .collect()
    }

    // ---- files ----

    pub fn blockstate(&self, name: &str) -> Result<Arc<BlockState>> {
        let name = normalize_location(name)?;
        if let Some(b) = self.blockstates.lock().unwrap().get(&name) {
            return Ok(b.clone());
        }
        let path = format!("{ROOT}/blockstates/{name}.json");
        if !self.jar.contains(&path) {
            bail!("no blockstate `{name}` in Minecraft {}", self.version.id);
        }
        let text = self.jar.read_json(&path)?;
        let b = Arc::new(BlockState::parse(&text).with_context(|| path.clone())?);
        self.blockstates.lock().unwrap().insert(name, b.clone());
        Ok(b)
    }

    /// A model file by its path relative to `models/` (`block/stone`), parsed, not resolved
    pub fn raw_model(&self, id: &str) -> Result<Arc<RawModel>> {
        if let Some(m) = self.raw_models.lock().unwrap().get(id) {
            return Ok(m.clone());
        }
        let path = format!("{ROOT}/models/{id}.json");
        let text = self.jar.read_json(&path)?;
        let m = Arc::new(RawModel::parse(&text).with_context(|| path.clone())?);
        self.raw_models
            .lock()
            .unwrap()
            .insert(id.to_string(), m.clone());
        Ok(m)
    }

    /// A model with its parents merged
    pub fn model(&self, id: &str) -> Result<Arc<ResolvedModel>> {
        let id = normalize_location(id)?;
        if let Some(m) = self.resolved.lock().unwrap().get(&id) {
            return Ok(m.clone());
        }
        let m = Arc::new(resolve(&id, &mut |p| self.raw_model(p))?);
        self.resolved.lock().unwrap().insert(id, m.clone());
        Ok(m)
    }

    /// The id of the model a blockstate variant names, as a path relative to `models/`
    pub fn variant_model_id(&self, model: &str) -> Result<String> {
        let m = normalize_location(model)?;
        Ok(if self.layout().blockstate_models_are_block_relative() {
            format!("block/{m}")
        } else {
            m
        })
    }

    // ---- textures ----

    /// A texture by its path relative to `textures/` (`block/stone`), with its `.mcmeta`.
    ///
    /// A path the jar has no PNG for may be one the game generates from an atlas definition (the
    /// armor-trim item layers, `trims/items/boots_trim_amethyst`): those are produced here the same
    /// way, from the base texture and the palettes.
    pub fn texture(&self, path: &str) -> Result<Arc<Texture>> {
        let path = normalize_location(path)?;
        if let Some(t) = self.textures.lock().unwrap().get(&path) {
            return Ok(t.clone());
        }
        let entry = format!("{ROOT}/textures/{path}.png");
        if !self.jar.contains(&entry) {
            if let Some(src) = self.permutations().get(&path) {
                let t = Arc::new(
                    self.permute(src)
                        .with_context(|| format!("generating {path}"))?,
                );
                self.textures.lock().unwrap().insert(path, t.clone());
                return Ok(t);
            }
            bail!("no texture `{path}` in Minecraft {}", self.version.id);
        }
        let img = decode_png(&self.jar.read_png(&entry)?).with_context(|| entry.clone())?;
        let meta_entry = format!("{entry}.mcmeta");
        let meta = if self.jar.contains(&meta_entry) {
            Some(
                McMeta::parse(self.jar.read_json(&meta_entry)?)
                    .with_context(|| meta_entry.clone())?,
            )
        } else {
            None
        };
        let t = Arc::new(Texture::new(img, meta.as_ref()).with_context(|| entry)?);
        self.textures.lock().unwrap().insert(path, t.clone());
        Ok(t)
    }

    fn permutations(&self) -> &HashMap<String, PermSource> {
        self.permutations.get_or_init(|| {
            let mut out = HashMap::new();
            let prefix = format!("{ROOT}/atlases/");
            for n in self
                .jar
                .names_under(&prefix)
                .filter(|n| n.ends_with(".json"))
            {
                let Ok(text) = self.jar.read_json(n) else {
                    continue;
                };
                let Ok(v) = crate::jar::parse_json(&text) else {
                    continue;
                };
                let Some(sources) = v.get("sources").and_then(|s| s.as_array()) else {
                    continue;
                };
                for s in sources {
                    let ty = s.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    if ty.strip_prefix("minecraft:").unwrap_or(ty) != "paletted_permutations" {
                        continue;
                    }
                    let (Some(key), Some(perms), Some(texs)) = (
                        s.get("palette_key").and_then(|k| k.as_str()),
                        s.get("permutations").and_then(|p| p.as_object()),
                        s.get("textures").and_then(|t| t.as_array()),
                    ) else {
                        continue;
                    };
                    let sep = s.get("separator").and_then(|x| x.as_str()).unwrap_or("_");
                    for t in texs.iter().filter_map(|t| t.as_str()) {
                        for (name, palette) in perms {
                            let Some(pal) = palette.as_str() else {
                                continue;
                            };
                            out.insert(
                                format!("{}{sep}{name}", t.strip_prefix("minecraft:").unwrap_or(t)),
                                PermSource {
                                    base: t.strip_prefix("minecraft:").unwrap_or(t).to_string(),
                                    palette_key: key
                                        .strip_prefix("minecraft:")
                                        .unwrap_or(key)
                                        .to_string(),
                                    palette: pal
                                        .strip_prefix("minecraft:")
                                        .unwrap_or(pal)
                                        .to_string(),
                                },
                            );
                        }
                    }
                }
            }
            out
        })
    }

    /// Builds a `paletted_permutations` texture. The game's rules for the awkward cases (a colour
    /// twice in the key palette, a key texel that is transparent, palettes of different sizes) are
    /// not in the data, so those palettes are refused rather than guessed at.
    fn permute(&self, src: &PermSource) -> Result<Texture> {
        let load = |p: &str| -> Result<crate::texture::Rgba> {
            decode_png(&self.jar.read_png(&format!("{ROOT}/textures/{p}.png"))?)
                .with_context(|| p.to_string())
        };
        let (key, pal, mut img) = (
            load(&src.palette_key)?,
            load(&src.palette)?,
            load(&src.base)?,
        );
        let n = key.data.len() / 4;
        if pal.data.len() / 4 != n {
            return Err(Refused(format!(
                "palette {} has {} colours and the key {} has {n}",
                src.palette,
                pal.data.len() / 4,
                src.palette_key
            ))
            .into());
        }
        if key.data.chunks_exact(4).any(|p| p[3] != 255)
            || pal.data.chunks_exact(4).any(|p| p[3] != 255)
        {
            return Err(Refused(format!(
                "palette {} or its key has transparent colours; what the game does with them is not in the data",
                src.palette
            ))
            .into());
        }
        // palette key colour -> replacement colour, by position in the two palettes
        let map: Vec<([u8; 3], [u8; 3])> = (0..n)
            .map(|i| {
                let k = &key.data[i * 4..i * 4 + 3];
                let p = &pal.data[i * 4..i * 4 + 3];
                ([k[0], k[1], k[2]], [p[0], p[1], p[2]])
            })
            .collect();
        for (i, (k, _)) in map.iter().enumerate() {
            if map[..i].iter().any(|(k2, _)| k2 == k) {
                return Err(Refused(format!(
                    "the key palette {} has a colour twice; which replacement the game picks is not in the data",
                    src.palette_key
                ))
                .into());
            }
        }
        for px in img.data.chunks_exact_mut(4) {
            if px[3] == 0 {
                continue;
            }
            if let Some((_, to)) = map.iter().find(|(k, _)| k[..] == px[..3]) {
                px[..3].copy_from_slice(to);
            }
        }
        Texture::new(img, None)
    }

    pub fn has_texture(&self, path: &str) -> bool {
        normalize_location(path)
            .map(|p| {
                self.jar.contains(&format!("{ROOT}/textures/{p}.png"))
                    || self.permutations().contains_key(&p)
            })
            .unwrap_or(false)
    }

    pub fn clear_caches(&self) {
        self.blockstates.lock().unwrap().clear();
        self.raw_models.lock().unwrap().clear();
        self.resolved.lock().unwrap().clear();
        self.textures.lock().unwrap().clear();
    }

    // ---- tints ----

    pub fn colormap(&self, kind: ColormapKind) -> Result<Arc<Colormap>> {
        if let Some(c) = self.colormaps.lock().unwrap().get(&kind) {
            return Ok(c.clone());
        }
        let entry = kind.file();
        if !self.jar.contains(entry) {
            bail!(
                "Minecraft {} has no {} colormap",
                self.version.id,
                kind.name()
            );
        }
        let c = Arc::new(Colormap::new(decode_png(&self.jar.read_png(entry)?)?)?);
        self.colormaps.lock().unwrap().insert(kind, c.clone());
        Ok(c)
    }

    /// The names of the biomes the jar's data describes (jars from 1.19 on; earlier biomes are code)
    pub fn biome_names(&self) -> Vec<String> {
        let prefix = "data/minecraft/worldgen/biome/";
        self.jar
            .names_under(prefix)
            .filter_map(|n| n.strip_prefix(prefix))
            .filter_map(|n| n.strip_suffix(".json"))
            .map(String::from)
            .collect()
    }

    pub fn biome(&self, name: &str) -> Result<Biome> {
        let name = normalize_location(name)?;
        let entry = format!("data/minecraft/worldgen/biome/{name}.json");
        if !self.jar.contains(&entry) {
            bail!(
                "no biome `{name}` in the data of Minecraft {} ({} biomes there)",
                self.version.id,
                self.biome_names().len()
            );
        }
        Biome::parse(&name, self.jar.read_json(&entry)?)
    }

    pub fn tint_rgb(&self, t: &Tint) -> Result<[u8; 3]> {
        match t {
            Tint::Rgb(c) => Ok(*c),
            Tint::Colormap {
                kind,
                temperature,
                downfall,
            } => Ok(self.colormap(*kind)?.lookup(*temperature, *downfall)),
        }
    }

    // ---- baking ----

    /// Does the model have anything to draw from the data? `None`: yes.
    pub fn left_out_of(&self, m: &ResolvedModel) -> Option<LeftOut> {
        if m.builtin == Some(Builtin::Entity) {
            Some(LeftOut::BuiltinEntity)
        } else if m.builtin == Some(Builtin::Generated) {
            None
        } else if m.elements.is_empty() {
            Some(LeftOut::NoElements)
        } else {
            None
        }
    }

    /// `Some(MissingTexture)` when no face of the model names a texture that can exist
    fn unnameable_only(&self, m: &ResolvedModel) -> Option<LeftOut> {
        if m.builtin.is_some() || m.elements.is_empty() {
            return None;
        }
        let any = m
            .elements
            .iter()
            .flat_map(|e| e.faces.iter())
            .any(|(_, f)| {
                m.texture(&f.texture)
                    .map(|t| t.path.contains('/') || self.has_texture(&t.path))
                    .unwrap_or(false)
            });
        if any {
            None
        } else {
            Some(LeftOut::MissingTexture)
        }
    }

    /// Bakes what an id names: `block/<name>[?variant=<state>]` or `item/<name>`
    pub fn bake_id(&self, id: &str) -> Result<BakedModel> {
        match AssetId::parse(id)? {
            AssetId::Block { name, variant } => {
                let state = State::parse(variant.as_deref().unwrap_or(""))?;
                self.bake_block(&name, &state)
            }
            AssetId::Item { name } => self.bake_item(&name),
        }
    }

    /// The tint for a colormap at a biome's climate, from the jar's own biome data. A colour the
    /// biome file overrides (`grass_color`, `foliage_color`, `dry_foliage_color`) is used as is. The
    /// second value lists what the game does in code and this does not (grass colour modifiers).
    pub fn tint_for_biome(&self, kind: ColormapKind, biome: &str) -> Result<(Tint, Vec<String>)> {
        let b = self.biome(biome)?;
        let mut notes = Vec::new();
        if let Some(m) = &b.grass_color_modifier {
            if kind == ColormapKind::Grass {
                notes.push(format!("{}: grass_color_modifier `{m}` is applied by the game's code and is not applied here", b.name));
            }
        }
        if let Some(m) = &b.temperature_modifier {
            notes.push(format!("{}: temperature_modifier `{m}` is applied by the game's code and is not applied here", b.name));
        }
        let over = match kind {
            ColormapKind::Grass => b.grass_color,
            ColormapKind::Foliage => b.foliage_color,
            ColormapKind::DryFoliage => b.dry_foliage_color,
        };
        Ok((
            match over {
                Some(c) => Tint::Rgb(c),
                None => Tint::Colormap {
                    kind,
                    temperature: b.temperature,
                    downfall: b.downfall,
                },
            },
            notes,
        ))
    }

    /// Drops the quads whose texture name has no folder (`all` where `#all` was meant, as in
    /// Mojang's `heavy_core` model): such a texture cannot exist, and the game draws its built-in
    /// missing-texture pattern there, which is code and not data. Returns the names dropped.
    fn drop_unnameable(&self, quads: &mut Vec<Quad>) -> Vec<String> {
        let mut dropped = Vec::new();
        quads.retain(|q| {
            if q.texture.contains('/') || self.has_texture(&q.texture) {
                true
            } else {
                if !dropped.contains(&q.texture) {
                    dropped.push(q.texture.clone());
                }
                false
            }
        });
        dropped
    }

    /// The state `bake_block` builds when none is given, as `?variant=` text: the blockstate entry
    /// it takes (the file's `normal` or empty one, else its first), or for a multipart block that
    /// needs properties the first state its conditions describe. `None`: the file has no choice
    /// to make (one entry without properties, or only unconditional parts).
    pub fn default_state(&self, name: &str) -> Result<Option<String>> {
        let bs = self.blockstate(name)?;
        let sel = bs.select(&State::default())?;
        let key = match (&sel.key, sel.chosen.is_empty() && bs.is_multipart()) {
            (Some(k), _) => Some(k.clone()),
            (None, true) => bs.first_applying_state().map(|s| s.to_key()),
            _ => None,
        };
        Ok(key.filter(|k| {
            !k.is_empty()
                && k != "normal"
                && k != "inventory"
                && (sel.candidates > 1 || sel.key.is_none())
        }))
    }

    /// Bakes a block in a state. An empty `state` takes the blockstate file's first/`normal`
    /// variant (see [`BlockState::select`]).
    pub fn bake_block(&self, name: &str, state: &State) -> Result<BakedModel> {
        self.require_models()?;
        let name = normalize_location(name)?;
        let bs = self.blockstate(&name)?;
        let mut sel = bs.select(state)?;
        // What the state says once the properties this file does not mention are set aside
        let (known, _) = bs.split_known(state);
        let mut derived = None;
        if known.is_empty() && sel.chosen.is_empty() && bs.is_multipart() {
            // Nothing unconditional applies: use the first state the file's own conditions describe
            if let Some(st) = bs.first_applying_state() {
                sel = bs.select(&st)?;
                derived = Some(st.to_key());
            }
        }
        let mut report = Report {
            state_key: sel.key.clone(),
            ignored_properties: sel.ignored.clone(),
            candidates: sel.candidates,
            ..Report::default()
        };
        report.state_used = match (&sel.key, &derived) {
            (Some(k), _) => Some(k.clone()),
            (None, Some(k)) => Some(k.clone()),
            (None, None) if !known.is_empty() => Some(known.to_key()),
            _ => None,
        }
        // the empty and `normal` entries of a variants file are "no properties"
        .filter(|k| !k.is_empty() && k != "normal" && k != "inventory");
        if !report.ignored_properties.is_empty() {
            report.notes.push(format!(
                "the blockstate does not mention {}: ignored",
                report.ignored_properties.join(", ")
            ));
        }
        if let Some(k) = derived {
            report.notes.push(format!(
                "no state given and no part is unconditional: used `{k}`, the first state the conditions describe"
            ));
        }
        if sel.candidates > 1 {
            report.notes.push(if known.is_empty() {
                format!(
                    "no state given: used the entry `{}`, the file's `normal`/empty one or else its first of {}",
                    sel.key.clone().unwrap_or_default(),
                    sel.candidates
                )
            } else {
                format!(
                    "the state fits {} variants; the first in the file was used (`{}`)",
                    sel.candidates,
                    sel.key.clone().unwrap_or_default()
                )
            });
        }
        let mut parts = Vec::new();
        let mut first_left_out = None;
        for v in &sel.chosen {
            let id = self.variant_model_id(&v.model)?;
            let m = self.model(&id)?;
            if let Some(lo) = self.left_out_of(&m) {
                first_left_out.get_or_insert(lo);
                continue;
            }
            let (mut quads, face_notes) = bake_model(
                &m,
                VariantRotation {
                    x: v.x,
                    y: v.y,
                    uvlock: v.uvlock,
                },
            )?;
            report.notes.extend(face_notes);
            let before = quads.len();
            for t in self.drop_unnameable(&mut quads) {
                if quads.is_empty() && before > 0 {
                    first_left_out.get_or_insert(LeftOut::MissingTexture);
                }
                report.notes.push(format!(
                    "{id}: faces that name texture `{t}` were left out (no such texture; the game draws its built-in missing-texture pattern)"
                ));
            }
            if !quads.is_empty() {
                parts.push(Part {
                    model: id,
                    quads,
                    tints: TintSet::new(),
                });
            }
        }
        if parts.is_empty() {
            report.left_out = Some(if sel.chosen.is_empty() {
                LeftOut::NoPartMatched
            } else {
                first_left_out.unwrap_or(LeftOut::NoElements)
            });
            // a block entity in 26.x: the item definition says so
            if let Some(LeftOut::NoElements) = report.left_out {
                if let Some(special) = self.special_item_kinds(&name) {
                    report.left_out = Some(LeftOut::SpecialItem(special));
                }
            }
        }
        Ok(BakedModel {
            id: format!("block/{name}"),
            parts,
            report,
        })
    }

    /// For 1.21.4 and later: the `special` model types of the item with this name, if it has any
    fn special_item_kinds(&self, name: &str) -> Option<Vec<String>> {
        if self.layout() != Layout::ModelsWithItemDefs {
            return None;
        }
        let d = self.item_definition(name).ok()?;
        let r = item::default_models(&d);
        if r.special.is_empty() {
            None
        } else {
            Some(r.special)
        }
    }

    pub fn item_definition(&self, name: &str) -> Result<Value> {
        let path = format!("{ROOT}/items/{name}.json");
        if !self.jar.contains(&path) {
            bail!(
                "no item definition `{name}` in Minecraft {}",
                self.version.id
            );
        }
        crate::jar::parse_json(&self.jar.read_json(&path)?).with_context(|| path)
    }

    /// The models an item uses by default: from its definition (1.21.4 and later) or
    /// `models/item/<name>.json`
    pub fn item_default_models(&self, name: &str) -> Result<DefaultModels> {
        self.require_models()?;
        let name = normalize_location(name)?;
        if self.layout() == Layout::ModelsWithItemDefs {
            Ok(item::default_models(&self.item_definition(&name)?))
        } else {
            if !self
                .jar
                .contains(&format!("{ROOT}/models/item/{name}.json"))
            {
                bail!("no item model `{name}` in Minecraft {}", self.version.id);
            }
            Ok(DefaultModels {
                models: vec![item::ItemModelRef {
                    model: format!("item/{name}"),
                    tints: Vec::new(),
                }],
                ..DefaultModels::default()
            })
        }
    }

    /// Bakes one item model (a path relative to `models/`): the generated flat mesh, or its
    /// elements like any block model
    pub fn bake_item_model(&self, model_id: &str) -> Result<(Vec<Quad>, Vec<String>)> {
        let m = self.model(model_id)?;
        match m.builtin {
            Some(Builtin::Generated) => {
                let layers = m.layers();
                if layers.is_empty() {
                    bail!("{}: builtin/generated with no layer textures", m.id);
                }
                let mut quads = Vec::new();
                let mut notes = Vec::new();
                for (i, _) in &layers {
                    let reference = format!("#layer{i}");
                    let tex = m.texture(&reference)?;
                    let t = self
                        .texture(&tex.path)
                        .with_context(|| format!("{} {reference}", m.id))?;
                    let (w, h, opaque) = item::opacity_union(&t);
                    for el in item::layer_elements(*i, &reference, w, h, &opaque)
                        .with_context(|| m.id.clone())?
                    {
                        bake_element(
                            &el,
                            VariantRotation::default(),
                            &mut |r| {
                                let tp = m.texture(r)?;
                                Ok((tp.path, tp.translucent))
                            },
                            &mut quads,
                            &mut notes,
                        )?;
                    }
                }
                Ok((quads, notes))
            }
            Some(Builtin::Entity) => Err(NotData(m.id.clone(), LeftOut::BuiltinEntity).into()),
            Some(Builtin::Other(ref o)) => Err(NotData(
                m.id.clone(),
                LeftOut::Refused(format!("built-in root `builtin/{o}` is not data")),
            )
            .into()),
            None => {
                if m.elements.is_empty() {
                    return Err(NotData(m.id.clone(), LeftOut::NoElements).into());
                }
                let (mut quads, mut notes) = bake_model(&m, VariantRotation::default())?;
                notes.extend(self.drop_unnameable(&mut quads).into_iter().map(|t| {
                    format!(
                        "{}: faces that name texture `{t}` were left out (no such texture; the game draws its built-in missing-texture pattern)",
                        m.id
                    )
                }));
                Ok((quads, notes))
            }
        }
    }

    /// Bakes an item by name (`diamond_sword`).
    ///
    /// An item may be several models (a composite). Each part that cannot be baked leaves a line in
    /// the report's notes: a part that is drawn by code (`special`, `builtin/entity`) or refused is
    /// "left out"; a part that fails (a missing texture) is also listed in
    /// [`Report::failed_parts`], which `check` counts as a failure.
    pub fn bake_item(&self, name: &str) -> Result<BakedModel> {
        let name = normalize_location(name)?;
        let def = self.item_default_models(&name)?;
        let mut report = Report {
            notes: def.notes.clone(),
            ..Report::default()
        };
        let mut parts = Vec::new();
        let mut first_err = None;
        let mut left_outs: Vec<LeftOut> = Vec::new();
        let mut all_faces_dropped = false;
        for r in &def.models {
            let id = normalize_location(&r.model)?;
            match self.bake_item_model(&id) {
                Ok((quads, notes)) => {
                    report.notes.extend(notes);
                    let mut tints = TintSet::new();
                    for (i, t) in r.tints.iter().enumerate() {
                        if let Some(t) = t.to_tint() {
                            tints.0.insert(i as u32, t);
                        }
                    }
                    if quads.is_empty() {
                        all_faces_dropped = true;
                        report.notes.push(format!("{id}: every face was left out"));
                    } else {
                        parts.push(Part {
                            model: id,
                            quads,
                            tints,
                        });
                    }
                }
                Err(e) => {
                    if let Some(nd) = e.downcast_ref::<NotData>() {
                        report.notes.push(format!("{id}: left out: {}", nd.1));
                        report.dropped_parts.push(format!("{id}: {}", nd.1));
                        left_outs.push(nd.1.clone());
                    } else if let Some(rf) = e.downcast_ref::<Refused>() {
                        report.notes.push(format!("{id}: left out: refused: {rf}"));
                        report.dropped_parts.push(format!("{id}: refused: {rf}"));
                        left_outs.push(LeftOut::Refused(rf.0.clone()));
                    } else {
                        report.notes.push(format!("{id}: FAILED: {e:#}"));
                        report.failed_parts.push(format!("{id}: {e:#}"));
                        first_err.get_or_insert(e);
                    }
                }
            }
        }
        if !parts.is_empty() {
            for k in &def.special {
                report.notes.push(format!(
                    "a `special` model (`{k}`) is drawn by the game's code and was left out"
                ));
                report.dropped_parts.push(format!("special model `{k}`"));
            }
        } else if !def.special.is_empty() {
            report.dropped_parts.clear();
            report.left_out = Some(LeftOut::SpecialItem(def.special.clone()));
        } else if let Some(lo) = left_outs.into_iter().next() {
            report.dropped_parts.clear();
            report.left_out = Some(lo);
        } else if all_faces_dropped {
            report.left_out = Some(LeftOut::MissingTexture);
        } else if let Some(e) = first_err {
            return Err(e);
        } else {
            report.left_out = Some(LeftOut::NoElements);
        }
        Ok(BakedModel {
            id: format!("item/{name}"),
            parts,
            report,
        })
    }

    /// Whether a block draws anything by data, without baking: `None` yes; `Some` why not.
    /// Every model the file mentions is looked at, so a multipart block counts as drawn if any
    /// part is.
    pub fn classify_block(&self, name: &str) -> Result<Option<LeftOut>> {
        self.require_models()?;
        let bs = self.blockstate(name)?;
        let mut first = None;
        for v in bs.all_variants() {
            let m = self.model(&self.variant_model_id(&v.model)?)?;
            match self.left_out_of(&m).or_else(|| self.unnameable_only(&m)) {
                None => return Ok(None),
                Some(lo) => {
                    first.get_or_insert(lo);
                }
            }
        }
        let mut lo = first.unwrap_or(LeftOut::NoElements);
        if lo == LeftOut::NoElements {
            // the block of the same name as an item the game draws in code is a block entity
            if let Some(s) = self.special_item_kinds(name) {
                lo = LeftOut::SpecialItem(s);
            } else if self.layout() != Layout::ModelsWithItemDefs {
                if let Ok(Some(LeftOut::BuiltinEntity)) = self.classify_item(name) {
                    lo = LeftOut::BuiltinEntity;
                }
            }
        }
        Ok(Some(lo))
    }

    /// The same for an item, without baking (no texture is decoded). `None`: it draws something.
    pub fn classify_item(&self, name: &str) -> Result<Option<LeftOut>> {
        let def = self.item_default_models(name)?;
        let mut first = None;
        for r in &def.models {
            let m = self.model(&normalize_location(&r.model)?)?;
            let lo = match m.builtin {
                Some(Builtin::Generated) if m.layers().is_empty() => Some(LeftOut::Template),
                Some(Builtin::Generated) => None,
                Some(Builtin::Entity) => Some(LeftOut::BuiltinEntity),
                Some(Builtin::Other(_)) => Some(LeftOut::NoElements),
                None if m.elements.is_empty() => Some(LeftOut::NoElements),
                None => self.unnameable_only(&m),
            };
            match lo {
                None => return Ok(None),
                Some(l) => {
                    first.get_or_insert(l);
                }
            }
        }
        if !def.special.is_empty() {
            return Ok(Some(LeftOut::SpecialItem(def.special)));
        }
        Ok(Some(first.unwrap_or(LeftOut::NoElements)))
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::jar::testjar::{png, JarBuilder};

    /// A tiny jar with a cube, a half-height stair-like model with a variant rotation, a flat item,
    /// and a block entity, in the 1.13 to 1.21.3 layout
    pub fn tiny_jar() -> Jar {
        let tex = png(16, 16, |x, y| [(x * 16) as u8, (y * 16) as u8, 0, 255]);
        let leaf = png(16, 16, |x, _| {
            if x < 8 {
                [0, 200, 0, 255]
            } else {
                [0, 0, 0, 0]
            }
        });
        JarBuilder::new()
            .add("assets/minecraft/blockstates/cube.json", r#"{"variants":{"":{"model":"minecraft:block/cube"}}}"#)
            .add(
                "assets/minecraft/blockstates/step.json",
                r#"{"variants":{"facing=north":{"model":"block/step"},"facing=east":{"model":"block/step","y":90,"uvlock":true}}}"#,
            )
            .add(
                "assets/minecraft/blockstates/fence.json",
                r#"{"multipart":[{"apply":{"model":"block/post"}},{"when":{"north":"true"},"apply":{"model":"block/arm"}}]}"#,
            )
            .add("assets/minecraft/blockstates/chest.json", r#"{"variants":{"":{"model":"block/chest"}}}"#)
            .add("assets/minecraft/blockstates/leaves.json", r#"{"variants":{"":{"model":"block/leaves"}}}"#)
            .add(
                "assets/minecraft/models/block/cube_all.json",
                r##"{"textures":{"particle":"#all"},"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{
                  "down":{"texture":"#all","cullface":"down"},"up":{"texture":"#all","cullface":"up"},
                  "north":{"texture":"#all","cullface":"north"},"south":{"texture":"#all","cullface":"south"},
                  "west":{"texture":"#all","cullface":"west"},"east":{"texture":"#all","cullface":"east"}}}]}"##,
            )
            .add("assets/minecraft/models/block/cube.json", r#"{"parent":"block/cube_all","textures":{"all":"block/t"}}"#)
            .add(
                "assets/minecraft/models/block/step.json",
                r##"{"textures":{"all":"minecraft:block/t"},"elements":[{"from":[0,0,0],"to":[16,8,16],"faces":{
                  "up":{"texture":"#all"},"north":{"texture":"#all"}}}]}"##,
            )
            .add(
                "assets/minecraft/models/block/post.json",
                r##"{"textures":{"all":"block/t"},"elements":[{"from":[6,0,6],"to":[10,16,10],"faces":{"north":{"texture":"#all"}}}]}"##,
            )
            .add(
                "assets/minecraft/models/block/arm.json",
                r##"{"textures":{"all":"block/t"},"elements":[{"from":[7,6,0],"to":[9,9,6],"faces":{"up":{"texture":"#all"}}}]}"##,
            )
            .add("assets/minecraft/models/block/chest.json", r#"{"textures":{"particle":"block/t"}}"#)
            .add(
                "assets/minecraft/models/block/leaves.json",
                r##"{"textures":{"all":"block/leaf"},"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{"up":{"texture":"#all","tintindex":0}}}]}"##,
            )
            .add("assets/minecraft/models/item/generated.json", r#"{"parent":"builtin/generated"}"#)
            .add("assets/minecraft/models/item/entity.json", r#"{"parent":"builtin/entity"}"#)
            .add("assets/minecraft/models/item/leaf.json", r#"{"parent":"item/generated","textures":{"layer0":"item/leaf"}}"#)
            .add("assets/minecraft/models/item/cube.json", r#"{"parent":"block/cube"}"#)
            .add("assets/minecraft/textures/block/t.png", &tex)
            .add("assets/minecraft/textures/block/leaf.png", &leaf)
            .add("assets/minecraft/textures/item/leaf.png", &leaf)
            .build()
    }

    pub fn tiny() -> Assets {
        Assets::from_jar(tiny_jar()).unwrap()
    }

    #[test]
    fn cube_has_six_quads_on_one_texture() {
        let a = tiny();
        assert_eq!(a.layout(), Layout::Models);
        let b = a.bake_block("cube", &State::default()).unwrap();
        assert_eq!(b.quad_count(), 6);
        assert_eq!(
            b.texture_paths().into_iter().collect::<Vec<_>>(),
            ["block/t"]
        );
        assert!(b.report.left_out.is_none());
    }

    #[test]
    fn variants_by_state_and_rotation() {
        let a = tiny();
        let n = a
            .bake_block("step", &State::parse("facing=north").unwrap())
            .unwrap();
        let e = a
            .bake_block("step", &State::parse("facing=east").unwrap())
            .unwrap();
        assert_eq!(n.quad_count(), 2);
        assert_eq!(e.quad_count(), 2);
        // y = 90 turns the north face to the east
        let nq = n
            .quads()
            .find(|q| q.face == crate::model::Dir::North)
            .unwrap();
        let eq = e
            .quads()
            .find(|q| q.face == crate::model::Dir::North)
            .unwrap();
        assert_eq!(nq.normal, [0.0, 0.0, -1.0]);
        assert_eq!(eq.normal, [1.0, 0.0, 0.0]);
        assert!(a
            .bake_block("step", &State::parse("facing=up").unwrap())
            .is_err());
    }

    #[test]
    fn multipart_adds_parts() {
        let a = tiny();
        let plain = a.bake_block("fence", &State::default()).unwrap();
        assert_eq!(plain.parts.len(), 1);
        let arm = a
            .bake_block("fence", &State::parse("north=true").unwrap())
            .unwrap();
        assert_eq!(arm.parts.len(), 2);
        assert_eq!(arm.parts[1].model, "block/arm");
    }

    #[test]
    fn a_model_without_elements_is_left_out_not_an_error() {
        let a = tiny();
        let b = a.bake_block("chest", &State::default()).unwrap();
        assert!(b.is_empty());
        assert_eq!(b.report.left_out, Some(LeftOut::NoElements));
        assert_eq!(
            a.classify_block("chest").unwrap(),
            Some(LeftOut::NoElements)
        );
        assert_eq!(a.classify_block("cube").unwrap(), None);
    }

    #[test]
    fn tint_index_is_exposed() {
        let a = tiny();
        let b = a.bake_block("leaves", &State::default()).unwrap();
        assert_eq!(b.tint_indices().into_iter().collect::<Vec<_>>(), vec![0]);
    }

    #[test]
    fn generated_item_is_a_slab_with_edges() {
        let a = tiny();
        let i = a.bake_item("leaf").unwrap();
        // the left half of the texture is opaque: front + back + left half: top and bottom runs
        // (one each), the west edge (one run) and the east edge at the half-way line (one run)
        assert_eq!(i.quad_count(), 2 + 4);
        assert_eq!(i.id, "item/leaf");
        let west = i
            .quads()
            .find(|q| q.face == crate::model::Dir::West)
            .unwrap();
        assert_eq!(west.pos[0][0], 0.0);
        let east = i
            .quads()
            .find(|q| q.face == crate::model::Dir::East)
            .unwrap();
        assert_eq!(east.pos[0][0], 0.5);
    }

    #[test]
    fn block_parent_item_and_entity_item() {
        let a = tiny();
        assert_eq!(a.bake_item("cube").unwrap().quad_count(), 6);
        let e = a.bake_item("entity").unwrap();
        assert!(e.is_empty());
        assert_eq!(e.report.left_out, Some(LeftOut::BuiltinEntity));
        // the template itself has no layers: a real error, not "left out"
        assert!(a.bake_item("generated").is_err());
        assert!(a.bake_item("missing").is_err());
    }

    #[test]
    fn names_and_missing_files() {
        let a = tiny();
        assert_eq!(
            a.block_names(),
            ["chest", "cube", "fence", "leaves", "step"]
        );
        assert!(a.item_names().contains(&"leaf".to_string()));
        assert!(a.blockstate("nope").is_err());
        assert!(a.texture("block/nope").is_err());
        assert!(a.texture("block/t").is_ok());
    }

    #[test]
    fn legacy_blockstates_are_block_relative() {
        let jar = JarBuilder::new()
            .add("assets/minecraft/blockstates/a.json", r#"{"variants":{"normal":{"model":"m"}}}"#)
            .add("assets/minecraft/models/block/m.json",
                 r##"{"textures":{"all":"blocks/t"},"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{"up":{"texture":"#all"}}}]}"##)
            .add("assets/minecraft/textures/blocks/t.png", png(2, 2, |_, _| [1, 2, 3, 255]))
            .build();
        let a = Assets::from_jar(jar).unwrap();
        assert_eq!(a.layout(), Layout::LegacyModels);
        let b = a.bake_block("a", &State::default()).unwrap();
        assert_eq!(b.parts[0].model, "block/m");
        assert_eq!(b.quads().next().unwrap().texture, "blocks/t");
        assert!(a.texture("blocks/t").is_ok());
    }

    #[test]
    fn jars_without_models_say_so() {
        let jar = JarBuilder::new()
            .add(
                "assets/minecraft/textures/blocks/stone.png",
                png(1, 1, |_, _| [0, 0, 0, 255]),
            )
            .build();
        let a = Assets::from_jar(jar).unwrap();
        assert_eq!(a.layout(), Layout::LooseTextures);
        let e = a
            .bake_block("stone", &State::default())
            .unwrap_err()
            .to_string();
        assert!(e.contains("textures only"), "{e}");
        assert!(a.texture("blocks/stone").is_ok());
    }

    #[test]
    fn not_a_client_jar_is_refused() {
        let jar = JarBuilder::new().add("META-INF/MANIFEST.MF", "x").build();
        assert!(Assets::from_jar(jar).is_err());
    }

    #[test]
    fn item_definitions_pick_the_default_model_and_its_tints() {
        let jar = JarBuilder::new()
            .add("assets/minecraft/blockstates/leaves.json", r#"{"variants":{"":{"model":"block/leaves"}}}"#)
            .add("assets/minecraft/items/leaves.json",
                 r#"{"model":{"type":"minecraft:model","model":"minecraft:block/leaves","tints":[{"type":"minecraft:constant","value":-12012264}]}}"#)
            .add("assets/minecraft/items/chest.json",
                 r#"{"model":{"type":"minecraft:special","base":"minecraft:item/chest","model":{"type":"minecraft:chest","texture":"minecraft:normal"}}}"#)
            .add("assets/minecraft/blockstates/chest.json", r#"{"variants":{"":{"model":"block/chest"}}}"#)
            .add("assets/minecraft/models/block/chest.json", r#"{"textures":{"particle":"block/t"}}"#)
            .add("assets/minecraft/models/block/leaves.json",
                 r##"{"textures":{"all":"block/t"},"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{"up":{"texture":"#all","tintindex":0}}}]}"##)
            .add("assets/minecraft/textures/block/t.png", png(2, 2, |_, _| [1, 2, 3, 255]))
            .build();
        let a = Assets::from_jar(jar).unwrap();
        assert_eq!(a.layout(), Layout::ModelsWithItemDefs);
        let i = a.bake_item("leaves").unwrap();
        assert_eq!(
            i.parts[0].tints.get(0),
            Some(&Tint::Rgb([0x48, 0xB5, 0x18]))
        );
        let c = a.bake_item("chest").unwrap();
        assert_eq!(
            c.report.left_out,
            Some(LeftOut::SpecialItem(vec!["chest".into()]))
        );
        // the block itself reports the same reason from its item definition
        assert_eq!(
            a.classify_block("chest").unwrap(),
            Some(LeftOut::SpecialItem(vec!["chest".into()]))
        );
    }

    // ---- review fixes ----

    const CUBE_FACES: &str = r##""faces":{"up":{"texture":"#all"},"north":{"texture":"#all"}}"##;

    fn defs_jar(extra: Vec<(&str, Vec<u8>)>) -> Jar {
        let mut b = JarBuilder::new()
            .add("assets/minecraft/blockstates/x.json", r#"{"variants":{"":{"model":"block/m"}}}"#)
            .add(
                "assets/minecraft/models/block/m.json",
                format!(r##"{{"textures":{{"all":"block/t"}},"elements":[{{"from":[0,0,0],"to":[16,16,16],{CUBE_FACES}}}]}}"##),
            )
            .add("assets/minecraft/models/item/generated.json", r#"{"parent":"builtin/generated"}"#)
            .add("assets/minecraft/textures/block/t.png", png(2, 2, |_, _| [1, 2, 3, 255]))
            .add("assets/minecraft/textures/item/ok.png", png(16, 16, |_, _| [1, 2, 3, 255]));
        for (n, d) in extra {
            b = b.add(n, d);
        }
        b.build()
    }

    #[test]
    fn a_failed_part_of_a_composite_is_a_note_and_a_failed_part() {
        let jar = defs_jar(vec![
            (
                "assets/minecraft/items/c.json",
                br#"{"model":{"type":"minecraft:composite","models":[
                {"type":"minecraft:model","model":"minecraft:item/ok"},
                {"type":"minecraft:model","model":"minecraft:item/bad"}]}}"#
                    .to_vec(),
            ),
            (
                "assets/minecraft/models/item/ok.json",
                br#"{"parent":"item/generated","textures":{"layer0":"item/ok"}}"#.to_vec(),
            ),
            // the second part names a texture that is not in the jar
            (
                "assets/minecraft/models/item/bad.json",
                br#"{"parent":"item/generated","textures":{"layer0":"item/missing"}}"#.to_vec(),
            ),
        ]);
        let a = Assets::from_jar(jar).unwrap();
        let b = a.bake_item("c").unwrap();
        assert_eq!(b.parts.len(), 1, "the good part is still there");
        assert_eq!(b.report.failed_parts.len(), 1);
        assert!(
            b.report.failed_parts[0].contains("item/bad"),
            "{:?}",
            b.report.failed_parts
        );
        assert!(
            b.report
                .notes
                .iter()
                .any(|n| n.contains("FAILED") && n.contains("item/bad")),
            "{:?}",
            b.report.notes
        );
        // and the sweep counts the item as failed, not ok
        let r = crate::check::run(&a).unwrap();
        assert_eq!(r.items.failed, 1, "{:?}", r.failures);
        assert!(
            r.failures.iter().any(|f| f.contains("item part c")),
            "{:?}",
            r.failures
        );
    }

    #[test]
    fn a_dropped_special_in_a_composite_is_a_note_and_counted() {
        let jar = defs_jar(vec![
            ("assets/minecraft/items/c.json", br#"{"model":{"type":"minecraft:composite","models":[
                {"type":"minecraft:model","model":"minecraft:item/ok"},
                {"type":"minecraft:special","base":"minecraft:item/chest","model":{"type":"minecraft:chest","texture":"minecraft:normal"}}]}}"#.to_vec()),
            ("assets/minecraft/models/item/ok.json", br#"{"parent":"item/generated","textures":{"layer0":"item/ok"}}"#.to_vec()),
        ]);
        let a = Assets::from_jar(jar).unwrap();
        let b = a.bake_item("c").unwrap();
        assert!(!b.is_empty());
        assert!(
            b.report
                .notes
                .iter()
                .any(|n| n.contains("`special`") && n.contains("chest")),
            "{:?}",
            b.report.notes
        );
        assert_eq!(b.report.dropped_parts.len(), 1);
        let r = crate::check::run(&a).unwrap();
        assert_eq!(r.items_partial, 1);
        assert_eq!(r.total_failed(), 0, "{:?}", r.failures);
    }

    #[test]
    fn a_state_of_unknown_properties_falls_back_to_the_default_and_lists_them() {
        let a = tiny();
        // `step` has only facing= keys; `waterlogged` means nothing to it
        let b = a
            .bake_block("step", &State::parse("waterlogged=true").unwrap())
            .unwrap();
        assert!(!b.is_empty());
        assert_eq!(b.report.ignored_properties, vec!["waterlogged"]);
        assert_eq!(b.report.state_used.as_deref(), Some("facing=north"));
        assert!(
            b.report.notes.iter().any(|n| n.contains("waterlogged")),
            "{:?}",
            b.report.notes
        );
        // a multipart block asked for nothing it knows takes the unconditional parts
        let f = a
            .bake_block("fence", &State::parse("power=3").unwrap())
            .unwrap();
        assert_eq!(f.parts.len(), 1);
        assert_eq!(f.report.ignored_properties, vec!["power"]);
    }

    #[test]
    fn the_default_state_is_named() {
        let a = tiny();
        assert_eq!(
            a.default_state("step").unwrap().as_deref(),
            Some("facing=north")
        );
        assert_eq!(a.default_state("cube").unwrap(), None);
        assert_eq!(a.default_state("fence").unwrap(), None);
    }

    #[test]
    fn a_texture_variable_nothing_defines_leaves_that_face_out_not_the_model() {
        let jar = JarBuilder::new()
            .add("assets/minecraft/blockstates/x.json", r#"{"variants":{"":{"model":"block/m"}}}"#)
            .add("assets/minecraft/models/block/m.json",
                 r##"{"textures":{"all":"block/t"},"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{"up":{"texture":"#all"},"down":{"texture":"#nowhere"}}}]}"##)
            .add("assets/minecraft/textures/block/t.png", png(2, 2, |_, _| [1, 2, 3, 255]))
            .build();
        let a = Assets::from_jar(jar).unwrap();
        let b = a.bake_block("x", &State::default()).unwrap();
        assert_eq!(b.quad_count(), 1);
        assert!(
            b.report
                .notes
                .iter()
                .any(|n| n.contains("down face") && n.contains("#nowhere")),
            "{:?}",
            b.report.notes
        );
        let r = crate::check::run(&a).unwrap();
        assert_eq!(r.total_failed(), 0, "{:?}", r.failures);
        assert_eq!(
            r.left_out_inside_models.len(),
            1,
            "{:?}",
            r.left_out_inside_models
        );
    }

    fn trim_jar(key: Vec<u8>, pal: Vec<u8>) -> Jar {
        JarBuilder::new()
            .add("assets/minecraft/blockstates/x.json", r#"{"variants":{"":{"model":"block/nothing"}}}"#)
            .add("assets/minecraft/atlases/armor_trims.json",
                 r#"{"sources":[{"type":"paletted_permutations","textures":["trims/items/boots_trim"],"palette_key":"trims/color_palettes/trim_palette","permutations":{"amethyst":"trims/color_palettes/amethyst"}}]}"#)
            .add("assets/minecraft/textures/trims/items/boots_trim.png", png(2, 2, |x, _| if x == 0 { [10, 10, 10, 255] } else { [20, 20, 20, 255] }))
            .add("assets/minecraft/textures/trims/color_palettes/trim_palette.png", key)
            .add("assets/minecraft/textures/trims/color_palettes/amethyst.png", pal)
            .add("assets/minecraft/models/item/generated.json", r#"{"parent":"builtin/generated"}"#)
            .add("assets/minecraft/models/item/b.json", r#"{"parent":"item/generated","textures":{"layer0":"trims/items/boots_trim_amethyst"}}"#)
            .build()
    }

    #[test]
    fn a_palette_swap_is_made_when_the_palettes_are_unambiguous() {
        let key = png(2, 1, |x, _| {
            if x == 0 {
                [10, 10, 10, 255]
            } else {
                [20, 20, 20, 255]
            }
        });
        let pal = png(2, 1, |x, _| {
            if x == 0 {
                [200, 0, 0, 255]
            } else {
                [0, 200, 0, 255]
            }
        });
        let a = Assets::from_jar(trim_jar(key, pal)).unwrap();
        let t = a.texture("trims/items/boots_trim_amethyst").unwrap();
        assert_eq!(t.sheet.pixel(0, 0), [200, 0, 0, 255]);
        assert_eq!(t.sheet.pixel(1, 1), [0, 200, 0, 255]);
    }

    #[test]
    fn ambiguous_palettes_are_refused_with_a_reason_not_truncated() {
        let key2 = png(2, 1, |x, _| {
            if x == 0 {
                [10, 10, 10, 255]
            } else {
                [20, 20, 20, 255]
            }
        });
        // a palette with a different number of colours
        let pal3 = png(3, 1, |_, _| [200, 0, 0, 255]);
        let a = Assets::from_jar(trim_jar(key2.clone(), pal3)).unwrap();
        let e = a.texture("trims/items/boots_trim_amethyst").unwrap_err();
        assert!(
            e.downcast_ref::<Refused>()
                .is_some_and(|r| r.0.contains("3 colours")),
            "{e:#}"
        );
        let b = a.bake_item("b").unwrap();
        assert!(
            matches!(b.report.left_out, Some(LeftOut::Refused(ref w)) if w.contains("3 colours")),
            "{:?}",
            b.report.left_out
        );
        // a key with the same colour twice: which replacement wins is not in the data
        let dup = png(2, 1, |_, _| [10, 10, 10, 255]);
        let pal = png(2, 1, |x, _| {
            if x == 0 {
                [200, 0, 0, 255]
            } else {
                [0, 200, 0, 255]
            }
        });
        let a = Assets::from_jar(trim_jar(dup, pal.clone())).unwrap();
        let e = a.texture("trims/items/boots_trim_amethyst").unwrap_err();
        assert!(
            e.downcast_ref::<Refused>()
                .is_some_and(|r| r.0.contains("twice")),
            "{e:#}"
        );
        // a transparent palette colour
        let tr = png(2, 1, |_, _| [200, 0, 0, 0]);
        let a = Assets::from_jar(trim_jar(key2, tr)).unwrap();
        assert!(a
            .texture("trims/items/boots_trim_amethyst")
            .unwrap_err()
            .downcast_ref::<Refused>()
            .is_some());
    }

    #[test]
    fn a_huge_generated_layer_is_left_out_with_a_reason() {
        // a checkerboard of alpha: one strip per pixel edge
        let checker = png(64, 64, |x, y| {
            if (x + y) % 2 == 0 {
                [1, 1, 1, 255]
            } else {
                [0, 0, 0, 0]
            }
        });
        let wide = png(200, 4, |_, _| [1, 1, 1, 255]);
        let jar = defs_jar(vec![
            (
                "assets/minecraft/models/item/checker.json",
                br#"{"parent":"item/generated","textures":{"layer0":"item/checker"}}"#.to_vec(),
            ),
            ("assets/minecraft/textures/item/checker.png", checker),
            (
                "assets/minecraft/models/item/wide.json",
                br#"{"parent":"item/generated","textures":{"layer0":"item/wide"}}"#.to_vec(),
            ),
            ("assets/minecraft/textures/item/wide.png", wide),
        ]);
        let a = Assets::from_jar(jar).unwrap();
        for n in ["checker", "wide"] {
            let b = a.bake_item(n).unwrap();
            assert!(b.is_empty(), "{n}");
            assert!(
                matches!(b.report.left_out, Some(LeftOut::Refused(_))),
                "{n}: {:?}",
                b.report.left_out
            );
        }
        let b = a.bake_item("checker").unwrap();
        assert!(b.report.left_out.unwrap().to_string().contains("elements"));
        // the sweep does not call that a failure
        let r = crate::check::run(&a).unwrap();
        assert_eq!(r.total_failed(), 0, "{:?}", r.failures);
    }

    #[test]
    fn the_first_listed_frame_is_what_a_model_gets() {
        // a 2 x 4 sheet of two frames: frame 0 red, frame 1 green; the animation shows frame 1 first
        let jar = JarBuilder::new()
            .add("assets/minecraft/blockstates/x.json", r#"{"variants":{"":{"model":"block/m"}}}"#)
            .add("assets/minecraft/models/block/m.json",
                 r##"{"textures":{"all":"block/t"},"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{"up":{"texture":"#all"}}}]}"##)
            .add("assets/minecraft/textures/block/t.png", png(2, 4, |_, y| if y < 2 { [255, 0, 0, 255] } else { [0, 255, 0, 255] }))
            .add("assets/minecraft/textures/block/t.png.mcmeta", r#"{"animation":{"frames":[1,0]}}"#)
            .build();
        let a = Assets::from_jar(jar).unwrap();
        let b = a.bake_block("x", &State::default()).unwrap();
        let g = crate::mesh::build(&a, &b, &crate::mesh::MeshOptions::default()).unwrap();
        assert_eq!(g[0].image.pixel(0, 0), [0, 255, 0, 255]);
        let o = crate::mesh::MeshOptions {
            frame: Some(0),
            ..Default::default()
        };
        let g = crate::mesh::build(&a, &b, &o).unwrap();
        assert_eq!(g[0].image.pixel(0, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn every_json_is_read_through_a_limit() {
        let big = vec![b' '; (crate::jar::MAX_JSON as usize) + 1];
        let mut json = br#"{"variants":{"":{"model":"block/m"}}}"#.to_vec();
        json.extend_from_slice(&big);
        let jar = JarBuilder::new()
            .add("assets/minecraft/blockstates/x.json", json)
            .build();
        let a = Assets::from_jar(jar).unwrap();
        let e = a.blockstate("x").unwrap_err();
        assert!(format!("{e:#}").contains("limit"), "{e:#}");
    }
}
