//! Biome names to the colours the colormaps give them, from the jar's own biome data (1.20.1 and later jars have
//! it; the 1.19.2 jar here has none, and before that biomes were in code, so there is nothing to read).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::assets::Assets;
use crate::tint::ColormapKind;

/// The id of "no biome known": a chunk that stores numeric biome ids, a name the jar has no data for
pub const NO_BIOME: u16 = 0;

/// Distinct biome names one load keeps (a game with every datapack has a few hundred)
pub const MAX_BIOMES: usize = 4096;

pub struct BiomeTable<'a> {
    assets: &'a Assets,
    pub names: Vec<String>,
    colors: Vec<[Option<[u8; 3]>; 3]>,
    index: HashMap<String, u16>,
    /// Biomes met whose `grass_color_modifier` or `temperature_modifier` the game applies in code:
    /// name to what it says. Their colours here are the plain colormap ones.
    pub modifiers: BTreeMap<String, String>,
    /// Names met that the jar has no biome data for
    pub unknown: BTreeSet<String>,
}

fn slot(k: ColormapKind) -> usize {
    match k {
        ColormapKind::Grass => 0,
        ColormapKind::Foliage => 1,
        ColormapKind::DryFoliage => 2,
    }
}

impl<'a> BiomeTable<'a> {
    pub fn new(assets: &'a Assets) -> Self {
        BiomeTable {
            assets,
            names: vec![String::new()],
            colors: vec![[None; 3]],
            index: HashMap::new(),
            modifiers: BTreeMap::new(),
            unknown: BTreeSet::new(),
        }
    }

    pub fn resolve(&mut self, name: &str) -> u16 {
        if let Some(&i) = self.index.get(name) {
            return i;
        }
        // a table this long is not a game's biome list; the rest are read as unknown
        if self.names.len() >= MAX_BIOMES {
            return NO_BIOME;
        }
        let mut colors = [None; 3];
        let known = match self.assets.biome(name) {
            Ok(b) => {
                for kind in [
                    ColormapKind::Grass,
                    ColormapKind::Foliage,
                    ColormapKind::DryFoliage,
                ] {
                    // a jar without a colormap (dry foliage before 1.21.5) leaves that colour out
                    if let Ok((tint, _)) = self.assets.tint_for_biome(kind, name) {
                        colors[slot(kind)] = self.assets.tint_rgb(&tint).ok();
                    }
                }
                let mut mods = Vec::new();
                if let Some(m) = &b.grass_color_modifier {
                    mods.push(format!("grass_color_modifier {m}"));
                }
                if let Some(m) = &b.temperature_modifier {
                    mods.push(format!("temperature_modifier {m}"));
                }
                if !mods.is_empty() {
                    self.modifiers.insert(name.to_string(), mods.join(", "));
                }
                true
            }
            Err(_) => false,
        };
        if !known {
            self.unknown.insert(name.to_string());
        }
        self.names.push(name.to_string());
        self.colors.push(colors);
        let id = (self.names.len() - 1).min(u16::MAX as usize) as u16;
        self.index.insert(name.to_string(), id);
        id
    }

    pub fn color(&self, id: u16, kind: ColormapKind) -> Option<[u8; 3]> {
        self.colors.get(id as usize).and_then(|c| c[slot(kind)])
    }
}
