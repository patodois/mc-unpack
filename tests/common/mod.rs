//! The gate of the tests that read real installs and saves.
//!
//! Looking for an install or a save reads the launcher folders of the machine the tests run on.
//! That is opt-in: nothing is looked for unless `MC_UNPACK_REAL_DATA=1` is set, and the tests that
//! need real data skip (and say how to turn them on) otherwise. The test files reach the search
//! through this module only; a test below holds them to that.

use std::ffi::OsStr;

use mc_unpack::install::JarFile;
use mc_unpack::world::save::SaveRef;

/// The variable that turns the real-data tests on; only the value `1` does
pub const REAL_DATA: &str = "MC_UNPACK_REAL_DATA";

fn enabled_by(value: Option<&OsStr>) -> bool {
    value.is_some_and(|v| v == "1")
}

/// Whether the real-data tests were asked for; says how to ask when they were not
pub fn real_data() -> bool {
    let on = enabled_by(std::env::var_os(REAL_DATA).as_deref());
    if !on {
        eprintln!(
            "skipping: tests on real installs and saves are opt-in. Set {REAL_DATA}=1 to run them: \
             they read (never write) the client jars and saves found in the launcher folders of this machine"
        );
    }
    on
}

/// `look` runs only when `on`: the gate comes before the search, not after it
fn gated<T: Default>(on: bool, look: impl FnOnce() -> T) -> T {
    if on {
        look()
    } else {
        T::default()
    }
}

/// The installed jar of `version`, when real data was asked for and it is installed
pub fn jar(version: &str) -> Option<JarFile> {
    let on = real_data();
    let jar = gated(on, || {
        mc_unpack::install::find_jars()
            .into_iter()
            .find(|j| j.version == version)
    });
    if on && jar.is_none() {
        eprintln!("skipping: Minecraft {version} is not installed here");
    }
    jar
}

/// The saves of this machine, when real data was asked for
#[allow(dead_code)]
pub fn saves() -> Vec<SaveRef> {
    gated(real_data(), mc_unpack::world::save::discover_saves)
}

#[test]
fn real_data_is_looked_for_only_when_asked() {
    assert!(!enabled_by(None));
    for no in ["", "0", "true", "yes", " 1", "11"] {
        assert!(!enabled_by(Some(OsStr::new(no))), "{no:?}");
    }
    assert!(enabled_by(Some(OsStr::new("1"))));
    // off: the search does not run at all
    let found: Vec<u8> = gated(false, || panic!("searched without being asked"));
    assert!(found.is_empty());
    assert_eq!(gated(true, || vec![1u8]), [1]);
    // and the test files have no way to the search but this module
    for (file, source) in [
        ("game_data.rs", include_str!("../game_data.rs")),
        ("world_data.rs", include_str!("../world_data.rs")),
    ] {
        for search in [
            "find_jars",
            "discover_saves",
            "with_discovered_saves",
            "open_default",
            "launcher_dirs",
            "jars_in",
            "save_folders_in",
            "prism_dirs",
            "minecraft_dirs",
            "prism_jars",
            "vanilla_jars",
        ] {
            assert!(
                !source.contains(search),
                "tests/{file} names `{search}`: real installs and saves are reached through tests/common only"
            );
        }
    }
}
