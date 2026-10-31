#![no_main]
//! `world::nbt::parse` on arbitrary bytes, and the text rendering of whatever it accepts
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok((name, root)) = mc_unpack::world::nbt::parse(data) {
        let _ = name;
        let _ = root.render(0);
        let _ = root.get("Data");
    }
});
