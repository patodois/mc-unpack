//! Where client jars live on a machine.
//!
//! Three kinds of place are searched; each is only read:
//!
//! - **Prism Launcher**: `<data>/libraries/com/mojang/minecraft/<version>/minecraft-<version>-client.jar`;
//! - **the vanilla launcher**: `<minecraft dir>/versions/<version>/<version>.jar`;
//! - an explicit path (`--jar`, or `MC_UNPACK_JAR`).
//!
//! | OS | Prism data folder | vanilla folder |
//! |----|-------------------|----------------|
//! | macOS | `~/Library/Application Support/PrismLauncher` | `~/Library/Application Support/minecraft` |
//! | Linux | `$XDG_DATA_HOME/PrismLauncher` (else `~/.local/share/PrismLauncher`) | `~/.minecraft` |
//! | Windows | `%APPDATA%\PrismLauncher` | `%APPDATA%\.minecraft` |
//!
//! `MC_UNPACK_PRISM_DIR` and `MC_UNPACK_MINECRAFT_DIR` replace the two folders. Only macOS has
//! been tried by the author; the Linux and Windows paths are the launchers' documented defaults.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Prism,
    Vanilla,
    Explicit,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Prism => "Prism Launcher",
            Source::Vanilla => "vanilla launcher",
            Source::Explicit => "given path",
        }
    }
}

#[derive(Clone, Debug)]
pub struct JarFile {
    /// The version the folder name gives; the jar's own `version.json` can say better
    pub version: String,
    pub path: PathBuf,
    pub source: Source,
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Candidate Prism data folders, most specific first
pub fn prism_dirs() -> Vec<PathBuf> {
    if let Some(p) = std::env::var_os("MC_UNPACK_PRISM_DIR") {
        return vec![PathBuf::from(p)];
    }
    let mut v = Vec::new();
    if let Some(h) = home() {
        v.push(h.join("Library/Application Support/PrismLauncher"));
        let xdg = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| h.join(".local/share"));
        v.push(xdg.join("PrismLauncher"));
    }
    if let Some(a) = std::env::var_os("APPDATA") {
        v.push(PathBuf::from(a).join("PrismLauncher"));
    }
    v
}

pub fn minecraft_dirs() -> Vec<PathBuf> {
    if let Some(p) = std::env::var_os("MC_UNPACK_MINECRAFT_DIR") {
        return vec![PathBuf::from(p)];
    }
    let mut v = Vec::new();
    if let Some(h) = home() {
        v.push(h.join("Library/Application Support/minecraft"));
        v.push(h.join(".minecraft"));
    }
    if let Some(a) = std::env::var_os("APPDATA") {
        v.push(PathBuf::from(a).join(".minecraft"));
    }
    v
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    v.sort();
    v
}

/// Jars under one Prism data folder
pub fn prism_jars(data: &Path) -> Vec<JarFile> {
    let root = data.join("libraries/com/mojang/minecraft");
    let mut out = Vec::new();
    for d in subdirs(&root) {
        let Some(v) = d.file_name().and_then(|s| s.to_str()).map(String::from) else {
            continue;
        };
        let jar = d.join(format!("minecraft-{v}-client.jar"));
        if jar.is_file() {
            out.push(JarFile {
                version: v,
                path: jar,
                source: Source::Prism,
            });
        }
    }
    out
}

/// Jars under one vanilla launcher folder. Modded profiles (Fabric, Forge) have a version folder
/// whose jar is not the game's; those are skipped when the jar is opened and has no assets.
pub fn vanilla_jars(dir: &Path) -> Vec<JarFile> {
    let root = dir.join("versions");
    let mut out = Vec::new();
    for d in subdirs(&root) {
        let Some(v) = d.file_name().and_then(|s| s.to_str()).map(String::from) else {
            continue;
        };
        let jar = d.join(format!("{v}.jar"));
        if jar.is_file() {
            out.push(JarFile {
                version: v,
                path: jar,
                source: Source::Vanilla,
            });
        }
    }
    out
}

/// Every jar found in the usual places, sorted by version name then source
pub fn find_jars() -> Vec<JarFile> {
    let mut out = Vec::new();
    for d in prism_dirs() {
        out.extend(prism_jars(&d));
    }
    for d in minecraft_dirs() {
        out.extend(vanilla_jars(&d));
    }
    if let Some(p) = std::env::var_os("MC_UNPACK_JAR") {
        let path = PathBuf::from(p);
        out.push(JarFile {
            version: crate::version::id_from_path(&path).unwrap_or_else(|| "unknown".into()),
            path,
            source: Source::Explicit,
        });
    }
    out.sort_by(|a, b| {
        version_key(&a.version)
            .cmp(&version_key(&b.version))
            .then(a.version.cmp(&b.version))
            .then((a.source as u8).cmp(&(b.source as u8)))
    });
    // The same file reached by two roots (a symlinked data folder) is listed once
    out.dedup_by(|a, b| a.path == b.path);
    out
}

/// Sorts `1.2.5` before `1.12.2` before `1.21.1` before `26.3`: numeric parts, in order
fn version_key(v: &str) -> Vec<u64> {
    v.split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().unwrap_or(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_order() {
        let mut v = vec!["26.3", "1.12.2", "1.2.5", "1.21.1", "1.7.10"];
        v.sort_by_key(|s| version_key(s));
        assert_eq!(v, ["1.2.5", "1.7.10", "1.12.2", "1.21.1", "26.3"]);
    }

    #[test]
    fn finds_both_layouts_in_a_temp_tree() {
        let base = std::env::temp_dir().join(format!("mc-unpack-install-{}", std::process::id()));
        let prism = base.join("prism");
        let van = base.join("van");
        for (dir, name) in [
            (prism.join("libraries/com/mojang/minecraft/1.20.1"), "minecraft-1.20.1-client.jar"),
            (van.join("versions/1.12.2"), "1.12.2.jar"),
        ] {
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(name), b"x").unwrap();
        }
        let p = prism_jars(&prism);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].version, "1.20.1");
        let v = vanilla_jars(&van);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].version, "1.12.2");
        fs::remove_dir_all(&base).unwrap();
    }
}
