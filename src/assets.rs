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
        let text = self.jar.read_string(&path)?;
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
        let text = self.jar.read_string(&path)?;
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
        let img = decode_png(&self.jar.read(&entry)?).with_context(|| entry.clone())?;
        let meta_entry = format!("{entry}.mcmeta");
        let meta = if self.jar.contains(&meta_entry) {
            Some(
                McMeta::parse(&self.jar.read_string(&meta_entry)?)
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
                let Ok(text) = self.jar.read_string(n) else {
                    continue;
                };
                let Ok(v) = serde_json::from_str::<Value>(&text) else {
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

    fn permute(&self, src: &PermSource) -> Result<Texture> {
        let load = |p: &str| -> Result<crate::texture::Rgba> {
            decode_png(&self.jar.read(&format!("{ROOT}/textures/{p}.png"))?)
                .with_context(|| p.to_string())
        };
        let (key, pal, mut img) = (
            load(&src.palette_key)?,
            load(&src.palette)?,
            load(&src.base)?,
        );
        let n = (key.width * key.height).min(pal.width * pal.height) as usize;
        // palette key colour -> replacement colour, by position in the two palettes
        let map: Vec<([u8; 3], [u8; 4])> = (0..n)
            .map(|i| {
                let k = &key.data[i * 4..i * 4 + 3];
                let p = &pal.data[i * 4..i * 4 + 4];
                ([k[0], k[1], k[2]], [p[0], p[1], p[2], p[3]])
            })
            .collect();
        for px in img.data.chunks_exact_mut(4) {
            if px[3] == 0 {
                continue;
            }
            if let Some((_, to)) = map.iter().find(|(k, _)| k[..] == px[..3]) {
                px[..3].copy_from_slice(&to[..3]);
                px[3] = ((px[3] as u32 * to[3] as u32) / 255) as u8;
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
        let c = Arc::new(Colormap::new(decode_png(&self.jar.read(entry)?)?)?);
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
        Biome::parse(&name, &self.jar.read_string(&entry)?)
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
                    .unwrap_or(true)
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

    /// Bakes a block in a state. An empty `state` takes the blockstate file's first/`normal`
    /// variant (see [`BlockState::select`]).
    pub fn bake_block(&self, name: &str, state: &State) -> Result<BakedModel> {
        self.require_models()?;
        let name = normalize_location(name)?;
        let bs = self.blockstate(&name)?;
        let mut sel = bs.select(state)?;
        let mut derived = None;
        if state.is_empty() && sel.chosen.is_empty() && bs.is_multipart() {
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
        if let Some(k) = derived {
            report.notes.push(format!(
                "no state given and no part is unconditional: used `{k}`, the first state the conditions describe"
            ));
        }
        if sel.candidates > 1 {
            report.notes.push(if state.is_empty() {
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
            let mut quads = bake_model(
                &m,
                VariantRotation {
                    x: v.x,
                    y: v.y,
                    uvlock: v.uvlock,
                },
            )?;
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
        serde_json::from_str(&self.jar.read_string(&path)?).with_context(|| path)
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
                for (i, _) in &layers {
                    let reference = format!("#layer{i}");
                    let tex = m.texture(&reference)?;
                    let t = self
                        .texture(&tex.path)
                        .with_context(|| format!("{} {reference}", m.id))?;
                    let (w, h, opaque) = item::opacity_union(&t);
                    for el in item::layer_elements(*i, &reference, w, h, &opaque) {
                        bake_element(
                            &el,
                            VariantRotation::default(),
                            &mut |r| {
                                let tp = m.texture(r)?;
                                Ok((tp.path, tp.translucent))
                            },
                            &mut quads,
                        )?;
                    }
                }
                Ok((quads, Vec::new()))
            }
            Some(Builtin::Entity) => bail!("{}: {}", m.id, LeftOut::BuiltinEntity),
            Some(Builtin::Other(ref o)) => {
                bail!("{}: built-in root `builtin/{o}` is not data", m.id)
            }
            None => {
                if m.elements.is_empty() {
                    bail!("{}: {}", m.id, LeftOut::NoElements);
                }
                let mut quads = bake_model(&m, VariantRotation::default())?;
                let notes = self
                    .drop_unnameable(&mut quads)
                    .into_iter()
                    .map(|t| {
                        format!(
                            "{}: faces that name texture `{t}` were left out (no such texture; the game draws its built-in missing-texture pattern)",
                            m.id
                        )
                    })
                    .collect();
                Ok((quads, notes))
            }
        }
    }

    /// Bakes an item by name (`diamond_sword`)
    pub fn bake_item(&self, name: &str) -> Result<BakedModel> {
        let name = normalize_location(name)?;
        let def = self.item_default_models(&name)?;
        let mut report = Report {
            notes: def.notes.clone(),
            ..Report::default()
        };
        let mut parts = Vec::new();
        let mut first_err = None;
        let mut missing_texture = false;
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
                        missing_texture = true;
                    } else {
                        parts.push(Part {
                            model: id,
                            quads,
                            tints,
                        });
                    }
                }
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        if parts.is_empty() {
            if !def.special.is_empty() {
                report.left_out = Some(LeftOut::SpecialItem(def.special.clone()));
            } else if missing_texture {
                report.left_out = Some(LeftOut::MissingTexture);
            } else if let Some(e) = first_err {
                // `builtin/entity` items and models without geometry are "left out"; anything
                // else (a missing texture) is a real failure
                let msg = format!("{e:#}");
                if msg.contains("builtin/entity") {
                    report.left_out = Some(LeftOut::BuiltinEntity);
                } else if msg.contains("no geometry") {
                    report.left_out = Some(LeftOut::NoElements);
                } else {
                    return Err(e);
                }
            } else {
                report.left_out = Some(LeftOut::NoElements);
            }
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
}
