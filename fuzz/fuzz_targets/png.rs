#![no_main]
//! A PNG and its `.mcmeta` to a texture with its animation. Input: a big-endian u16 length of the
//! PNG, the PNG, then the `.mcmeta` text.
use libfuzzer_sys::fuzz_target;
use mc_unpack::texture::{decode_png, McMeta, Texture};

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let n = (u16::from_be_bytes([data[0], data[1]]) as usize).min(data.len() - 2);
    let (png, meta) = data[2..].split_at(n);
    let sheet = decode_png(png);
    let meta = McMeta::parse(meta);
    if let Ok(sheet) = sheet {
        if let Ok(t) = Texture::new(sheet, meta.as_ref().ok()) {
            let _ = t.sheet_frames();
            let _ = t.default_frame();
            let _ = t.shown_frames();
            let _ = t.all_frames();
        }
    }
});
