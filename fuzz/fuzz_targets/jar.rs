#![no_main]
//! A client jar of arbitrary bytes: the zip directory, then every entry read through each of the
//! bounded readers, and the version detection.
use libfuzzer_sys::fuzz_target;
use mc_unpack::jar::Jar;
use mc_unpack::version;

fuzz_target!(|data: &[u8]| {
    let Ok(jar) = Jar::from_bytes(data.to_vec()) else {
        return;
    };
    let _ = jar.skipped_names();
    let names: Vec<String> = jar.names().iter().take(64).cloned().collect();
    for n in &names {
        let _ = jar.contains(n);
        let _ = jar.read_limited(n, 1 << 16);
        let _ = jar.read_json(n);
        let _ = jar.read_png(n);
    }
    let _ = jar.names_under("assets/").count();
    let _ = version::detect(&jar, None);
});
