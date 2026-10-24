//! Tinting: the biome colormaps and the lookup the game does in them.
//!
//! A face with a `tintindex` is multiplied by a colour the game picks at run time. Which colour
//! belongs to which block (grass blocks use the grass colormap, oak leaves the foliage one, redstone
//! dust a ramp by power, spruce and birch leaves fixed colours) is **in the game's code**, not in
//! the data, except in 26.x item definitions, which carry their own `tints`. This module therefore
//! does not guess: it exposes the tint index on every face, reads `grass.png`, `foliage.png` (and
//! `dry_foliage.png` where the jar has it) and does the lookup the game does for a temperature and
//! a downfall, and lets the caller say which index gets which colour ([`TintSet`]).
//!
//! The lookup (the game's `GrassColor`/`FoliageColor`, identical in 1.7.10 and 26.3): temperature
//! and downfall are clamped to 0..1, downfall is multiplied by temperature, and the colour is the
//! pixel at `x = (1 - temperature) * 255`, `y = (1 - downfall) * 255` (truncated) of the 256 x 256
//! map. No biome colour is hardcoded here: temperature and downfall come from the caller or, in
//! jars from 1.19 on, from the jar's own `data/minecraft/worldgen/biome/*.json` ([`Biome`]).

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::texture::Rgba;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ColormapKind {
    Grass,
    Foliage,
    /// Only in jars from 1.21.5 on
    DryFoliage,
}

impl ColormapKind {
    pub fn file(self) -> &'static str {
        match self {
            ColormapKind::Grass => "assets/minecraft/textures/colormap/grass.png",
            ColormapKind::Foliage => "assets/minecraft/textures/colormap/foliage.png",
            ColormapKind::DryFoliage => "assets/minecraft/textures/colormap/dry_foliage.png",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "grass" => Some(ColormapKind::Grass),
            "foliage" => Some(ColormapKind::Foliage),
            "dry_foliage" | "dry-foliage" => Some(ColormapKind::DryFoliage),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ColormapKind::Grass => "grass",
            ColormapKind::Foliage => "foliage",
            ColormapKind::DryFoliage => "dry_foliage",
        }
    }
}

/// A 256 x 256 colormap
#[derive(Clone, Debug)]
pub struct Colormap {
    pub image: Rgba,
}

impl Colormap {
    pub fn new(image: Rgba) -> Result<Colormap> {
        if image.width != 256 || image.height != 256 {
            bail!(
                "a colormap is 256 x 256, this one is {} x {}",
                image.width,
                image.height
            );
        }
        Ok(Colormap { image })
    }

    /// The colour for a temperature and a downfall, as the game computes it. Both are clamped to
    /// 0..1 first, so out-of-range biome values are safe.
    pub fn lookup(&self, temperature: f64, downfall: f64) -> [u8; 3] {
        let t = temperature.clamp(0.0, 1.0);
        let d = downfall.clamp(0.0, 1.0) * t;
        let x = ((1.0 - t) * 255.0) as u32;
        let y = ((1.0 - d) * 255.0) as u32;
        let p = self.image.pixel(x.min(255), y.min(255));
        [p[0], p[1], p[2]]
    }
}

/// What colour a tint index gets
#[derive(Clone, Debug, PartialEq)]
pub enum Tint {
    Rgb([u8; 3]),
    Colormap {
        kind: ColormapKind,
        temperature: f64,
        downfall: f64,
    },
}

/// A colour per tint index. An index without an entry is left untinted (white).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TintSet(pub BTreeMap<u32, Tint>);

impl TintSet {
    pub fn new() -> Self {
        TintSet::default()
    }

    pub fn with(mut self, index: u32, tint: Tint) -> Self {
        self.0.insert(index, tint);
        self
    }

    pub fn get(&self, index: u32) -> Option<&Tint> {
        self.0.get(&index)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Parses `#rrggbb`, `rrggbb`, or `0xrrggbb`
pub fn parse_rgb(s: &str) -> Result<[u8; 3]> {
    let h = s
        .trim()
        .trim_start_matches('#')
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    if h.len() != 6 {
        bail!("{s:?}: expected 6 hex digits");
    }
    let v = u32::from_str_radix(h, 16).with_context(|| format!("{s:?} is not hex"))?;
    Ok([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

/// A colour as the data writes it: an integer (`0xRRGGBB`, or a signed ARGB such as the
/// `-12012264` of an item tint) or a `"#rrggbb"` string
pub fn color_from_json(v: &Value) -> Option<[u8; 3]> {
    match v {
        Value::Number(n) => {
            let i = n.as_i64()?;
            let u = i as u32;
            Some([(u >> 16) as u8, (u >> 8) as u8, u as u8])
        }
        Value::String(s) => parse_rgb(s).ok(),
        _ => None,
    }
}

/// A biome's climate and colour overrides, as the jar's data gives them (jars from 1.19 on; before
/// that biomes were in code)
#[derive(Clone, Debug, PartialEq)]
pub struct Biome {
    pub name: String,
    pub temperature: f64,
    pub downfall: f64,
    pub grass_color: Option<[u8; 3]>,
    pub foliage_color: Option<[u8; 3]>,
    pub dry_foliage_color: Option<[u8; 3]>,
    /// `swamp`, `dark_forest` ...: a rule the game applies in code, which is **not** applied here
    pub grass_color_modifier: Option<String>,
    /// `frozen` ...: also applied in code and not here
    pub temperature_modifier: Option<String>,
}

impl Biome {
    pub fn parse(name: &str, text: &str) -> Result<Biome> {
        let v: Value = serde_json::from_str(text).with_context(|| format!("biome {name}"))?;
        let temperature = v
            .get("temperature")
            .and_then(|n| n.as_f64())
            .with_context(|| format!("biome {name}: no temperature"))?;
        let downfall = v
            .get("downfall")
            .and_then(|n| n.as_f64())
            .with_context(|| format!("biome {name}: no downfall"))?;
        let eff = v.get("effects");
        let col = |k: &str| eff.and_then(|e| e.get(k)).and_then(color_from_json);
        let s = |o: Option<&Value>, k: &str| {
            o.and_then(|e| e.get(k))
                .and_then(|x| x.as_str())
                .map(String::from)
        };
        Ok(Biome {
            name: name.to_string(),
            temperature,
            downfall,
            grass_color: col("grass_color"),
            foliage_color: col("foliage_color"),
            dry_foliage_color: col("dry_foliage_color"),
            grass_color_modifier: s(eff, "grass_color_modifier"),
            temperature_modifier: s(Some(&v), "temperature_modifier"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic map whose pixel (x, y) is (x, y, 7)
    fn ramp() -> Colormap {
        let mut data = Vec::new();
        for y in 0..256u32 {
            for x in 0..256u32 {
                data.extend_from_slice(&[x as u8, y as u8, 7, 255]);
            }
        }
        Colormap::new(Rgba {
            width: 256,
            height: 256,
            data,
        })
        .unwrap()
    }

    #[test]
    fn lookup_corners() {
        let m = ramp();
        // temperature 1, downfall 1: x = 0, y = 0
        assert_eq!(m.lookup(1.0, 1.0), [0, 0, 7]);
        // temperature 0: x = 255, and downfall is multiplied by 0, so y = 255
        assert_eq!(m.lookup(0.0, 1.0), [255, 255, 7]);
        // temperature 1, downfall 0: x = 0, y = 255
        assert_eq!(m.lookup(1.0, 0.0), [0, 255, 7]);
    }

    #[test]
    fn lookup_truncates_and_multiplies() {
        let m = ramp();
        // t = 0.5 -> x = 127.5 -> 127; d = 1.0 * 0.5 = 0.5 -> y = 127
        assert_eq!(m.lookup(0.5, 1.0), [127, 127, 7]);
        // t = 0.8, rain 0.4 -> d = 0.32 -> x = (1 - 0.8) * 255 = 50.99999... -> 50 (truncated, not
        // rounded), y = 173.4 -> 173
        assert_eq!(m.lookup(0.8, 0.4), [50, 173, 7]);
    }

    #[test]
    fn lookup_clamps() {
        let m = ramp();
        assert_eq!(m.lookup(2.0, 5.0), m.lookup(1.0, 1.0));
        assert_eq!(m.lookup(-1.0, -1.0), m.lookup(0.0, 0.0));
    }

    #[test]
    fn wrong_size_is_refused() {
        let small = Rgba {
            width: 16,
            height: 16,
            data: vec![0; 16 * 16 * 4],
        };
        assert!(Colormap::new(small).is_err());
    }

    #[test]
    fn colours_from_data() {
        // the signed ARGB of an item tint
        assert_eq!(
            color_from_json(&serde_json::json!(-12012264)),
            Some([0x48, 0xB5, 0x18])
        );
        assert_eq!(
            color_from_json(&serde_json::json!("#3f76e4")),
            Some([0x3f, 0x76, 0xe4])
        );
        assert_eq!(parse_rgb("0x00ff80").unwrap(), [0, 255, 128]);
        assert!(parse_rgb("fff").is_err());
    }

    #[test]
    fn biome_parse_both_colour_styles() {
        let b = Biome::parse(
            "swamp",
            r##"{"temperature":0.8,"downfall":0.9,"effects":{"grass_color_modifier":"swamp","water_color":6388580,"foliage_color":"#6a7039"}}"##,
        )
        .unwrap();
        assert_eq!(b.foliage_color, Some([0x6a, 0x70, 0x39]));
        assert_eq!(b.grass_color_modifier.as_deref(), Some("swamp"));
        assert_eq!(b.grass_color, None);
    }
}
