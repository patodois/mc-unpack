#![no_main]
//! The JSON and text parsers: blockstates, models, biomes, item definitions, `.mcmeta`, state
//! strings, resource locations, colours, `version.json`. The first byte picks the parser.
use libfuzzer_sys::fuzz_target;
use mc_unpack::blockstate::{BlockState, State};
use mc_unpack::model::{normalize_location, RawModel};
use mc_unpack::texture::McMeta;
use mc_unpack::tint::{parse_rgb, Biome};

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else {
        return;
    };
    let text = String::from_utf8_lossy(rest);
    match sel % 9 {
        0 => {
            if let Ok(bs) = BlockState::parse(rest) {
                // select against a few states, including the empty one
                for s in ["", "facing=east,half=bottom", "normal", "a=b,a=c"] {
                    if let Ok(st) = State::parse(s) {
                        let _ = bs.select(&st);
                    }
                }
            }
        }
        1 => {
            let _ = RawModel::parse(rest);
        }
        2 => {
            let _ = Biome::parse("fuzz", rest);
        }
        3 => {
            if let Ok(v) = mc_unpack::jar::parse_json(rest) {
                let _ = mc_unpack::item::default_models(&v);
                let _ = mc_unpack::tint::color_from_json(&v);
            }
        }
        4 => {
            let _ = McMeta::parse(rest);
        }
        5 => {
            let _ = State::parse(&text);
        }
        6 => {
            let _ = normalize_location(&text);
            let _ = mc_unpack::assets::AssetId::parse(&text);
        }
        7 => {
            let _ = parse_rgb(&text);
        }
        _ => {
            let _ = mc_unpack::version::id_from_path(std::path::Path::new(&*text));
        }
    }
});
