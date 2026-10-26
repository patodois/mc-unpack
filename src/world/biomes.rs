//! Biome names to the colours the colormaps give them, from the jar's own biome data (jars from 1.19
//! on; before that biomes were in code and there is nothing to read).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::blocks::TintSource;
use crate::assets::Assets;
use crate::tint::ColormapKind;

/// The id of "no biome known": a chunk that stores numeric biome ids, a name the jar has no data for
pub const NO_BIOME: u16 = 0;

pub struct BiomeTable<'a> {
    assets: &'a Assets,
    pub names: Vec<String>,
    colors: Vec<[Option<[u8; 3]>; 4]>,
    index: HashMap<String, u16>,
    /// Biomes met whose `grass_color_modifier` or `temperature_modifier` the game applies in code:
    /// name to what it says. Their colours here are the plain colormap ones.
    pub modifiers: BTreeMap<String, String>,
    /// Names met that the jar has no biome data for
    pub unknown: BTreeSet<String>,
}

fn slot(s: TintSource) -> usize {
    match s {
        TintSource::Colormap(ColormapKind::Grass) => 0,
        TintSource::Colormap(ColormapKind::Foliage) => 1,
        TintSource::Colormap(ColormapKind::DryFoliage) => 2,
        TintSource::Water => 3,
    }
}

impl<'a> BiomeTable<'a> {
    pub fn new(assets: &'a Assets) -> Self {
        BiomeTable {
            assets,
            names: vec![String::new()],
            colors: vec![[None; 4]],
            index: HashMap::new(),
            modifiers: BTreeMap::new(),
            unknown: BTreeSet::new(),
        }
    }

    pub fn resolve(&mut self, name: &str) -> u16 {
        if let Some(&i) = self.index.get(name) {
            return i;
        }
        let mut colors = [None; 4];
        let known = match self.assets.biome(name) {
            Ok(b) => {
                for kind in [
                    ColormapKind::Grass,
                    ColormapKind::Foliage,
                    ColormapKind::DryFoliage,
                ] {
                    // a jar without a colormap (dry foliage before 1.21.5) leaves that colour out
                    if let Ok((tint, _)) = self.assets.tint_for_biome(kind, name) {
                        colors[slot(TintSource::Colormap(kind))] = self.assets.tint_rgb(&tint).ok();
                    }
                }
                colors[slot(TintSource::Water)] = b.water_color;
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

    pub fn color(&self, id: u16, source: TintSource) -> Option<[u8; 3]> {
        self.colors.get(id as usize).and_then(|c| c[slot(source)])
    }
}
