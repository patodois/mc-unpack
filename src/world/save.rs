//! A save folder: where its region files are, which version it was saved by, and where saves are
//! looked for on a machine. Everything here only reads.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::level::{read_level_dat, LevelInfo};
use super::region::parse_region_name;
use crate::install;

/// A save folder that has a readable `level.dat`
#[derive(Clone, Debug)]
pub struct World {
    pub dir: PathBuf,
    pub level: LevelInfo,
}

/// `overworld`, `minecraft:the_nether` and `the_end` name the game's three; `namespace:name` (or
/// `namespace/name`) any other, as 1.16 and later datapacks add them
pub fn normalize_dimension(d: &str) -> Result<(String, String)> {
    let d = d.trim();
    let (ns, name) = match d.split_once([':', '/']) {
        Some((ns, name)) => (ns, name),
        None => ("minecraft", d),
    };
    let ok = |s: &str| {
        // no leading dot: `..` would walk out of the save folder
        !s.is_empty()
            && s.len() <= 64
            && !s.starts_with('.')
            && s.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-' | b'.')
            })
    };
    if !ok(ns) || !ok(name) {
        bail!("{d:?} is not a dimension name: lower-case letters, digits, `_`, `-`, `.`, as `overworld` or `minecraft:the_nether`");
    }
    Ok((ns.to_string(), name.to_string()))
}

impl World {
    pub fn open(dir: &Path) -> Result<World> {
        let lv = dir.join("level.dat");
        if !lv.is_file() {
            bail!(
                "{} has no level.dat: it is not a save folder",
                dir.display()
            );
        }
        let level = read_level_dat(&lv)?;
        Ok(World {
            dir: dir.to_path_buf(),
            level,
        })
    }

    /// The folder's name
    pub fn name(&self) -> String {
        self.dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Where a dimension's region files are: `dimensions/<namespace>/<name>/region` (1.16 and
    /// later, and the only place in 26.x), else the folders older saves use for the three the game
    /// has: `region`, `DIM-1/region`, `DIM1/region`
    pub fn region_dir(&self, dimension: &str) -> Result<PathBuf> {
        let (ns, name) = normalize_dimension(dimension)?;
        let modern = self
            .dir
            .join("dimensions")
            .join(&ns)
            .join(&name)
            .join("region");
        if modern.is_dir() {
            return Ok(modern);
        }
        let legacy = if ns == "minecraft" {
            match name.as_str() {
                "overworld" => Some(self.dir.join("region")),
                "the_nether" => Some(self.dir.join("DIM-1").join("region")),
                "the_end" => Some(self.dir.join("DIM1").join("region")),
                _ => None,
            }
        } else {
            None
        };
        match legacy {
            Some(p) if p.is_dir() => Ok(p),
            _ => bail!(
                "{}: no region folder for dimension {ns}:{name} (looked for {}{})",
                self.dir.display(),
                modern.display(),
                if ns == "minecraft" {
                    ", and the older region / DIM-1/region / DIM1/region"
                } else {
                    ""
                }
            ),
        }
    }

    /// The dimensions that have region files, `minecraft:overworld` first
    pub fn dimensions(&self) -> Vec<String> {
        let mut out = Vec::new();
        for d in ["overworld", "the_nether", "the_end"] {
            if self.region_dir(d).is_ok() {
                out.push(format!("minecraft:{d}"));
            }
        }
        let root = self.dir.join("dimensions");
        if let Ok(nss) = fs::read_dir(&root) {
            let mut extra = Vec::new();
            for ns in nss.flatten() {
                let Some(nsn) = ns.file_name().to_str().map(String::from) else {
                    continue;
                };
                if let Ok(names) = fs::read_dir(ns.path()) {
                    for n in names.flatten() {
                        let Some(nn) = n.file_name().to_str().map(String::from) else {
                            continue;
                        };
                        let id = format!("{nsn}:{nn}");
                        if n.path().join("region").is_dir() && !out.contains(&id) {
                            extra.push(id);
                        }
                    }
                }
            }
            extra.sort();
            out.extend(extra);
        }
        out
    }

    /// The regions of a dimension that have a file, sorted by z then x
    pub fn regions(&self, dimension: &str) -> Result<Vec<(i32, i32)>> {
        let dir = self.region_dir(dimension)?;
        let mut out = Vec::new();
        for e in fs::read_dir(&dir)
            .with_context(|| format!("reading {}", dir.display()))?
            .flatten()
        {
            if let Some((x, z)) = e.file_name().to_str().and_then(parse_region_name) {
                // an empty file is a region with no chunks
                if e.metadata().map(|m| m.len() > 0).unwrap_or(false) {
                    out.push((x, z));
                }
            }
        }
        out.sort_by_key(|&(x, z)| (z, x));
        Ok(out)
    }
}

/// A save found on this machine
#[derive(Clone, Debug)]
pub struct SaveRef {
    /// What an id calls it: the folder name, or `instance/folder` when two saves share a folder name
    pub label: String,
    pub path: PathBuf,
    /// The launcher instance it belongs to, where there is one (Prism)
    pub instance: Option<String>,
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Saves in the launchers' folders (read only): Prism's `instances/*/minecraft/saves` (and
/// `.minecraft/saves`), the vanilla launcher's `saves`, and the folders in `MC_UNPACK_SAVES`
/// (separated like `PATH`)
pub fn discover_saves() -> Vec<SaveRef> {
    let mut found: Vec<(String, Option<String>, PathBuf)> = Vec::new();
    let mut add_dir = |saves: &Path, instance: Option<String>| {
        for s in subdirs(saves) {
            if s.join("level.dat").is_file() {
                let folder = s
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                found.push((folder, instance.clone(), s));
            }
        }
    };
    for prism in install::prism_dirs() {
        for inst in subdirs(&prism.join("instances")) {
            let iname = inst
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            for mc in ["minecraft", ".minecraft"] {
                add_dir(&inst.join(mc).join("saves"), Some(iname.clone()));
            }
        }
    }
    for mc in install::minecraft_dirs() {
        add_dir(&mc.join("saves"), None);
    }
    if let Some(extra) = std::env::var_os("MC_UNPACK_SAVES") {
        for d in std::env::split_paths(&extra) {
            add_dir(&d, None);
        }
    }
    found.sort_by(|a, b| a.2.cmp(&b.2));
    found.dedup_by(|a, b| a.2 == b.2);
    let mut out = Vec::new();
    for (folder, instance, path) in &found {
        let dup = found.iter().filter(|f| &f.0 == folder).count() > 1;
        let label = match (dup, instance) {
            (true, Some(i)) => format!("{i}/{folder}"),
            _ => folder.clone(),
        };
        out.push(SaveRef {
            label,
            path: path.clone(),
            instance: instance.clone(),
        });
    }
    // two saves that still share a label (the same instance name twice, say) are told apart by index
    let mut seen = std::collections::HashMap::new();
    for s in &mut out {
        let n = seen.entry(s.label.clone()).or_insert(0usize);
        *n += 1;
        if *n > 1 {
            s.label = format!("{}#{}", s.label, n);
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::world::nbt::write::*;
    use crate::world::nbt::Tag;
    use crate::world::region::tests::temp_dir;

    pub fn level_dat(version: &str, data_version: i32) -> Vec<u8> {
        let root = compound(vec![(
            "Data",
            compound(vec![
                ("DataVersion", Tag::Int(data_version)),
                ("Version", compound(vec![("Name", string(version))])),
                ("LevelName", string("T")),
            ]),
        )]);
        crate::world::compress::tests::gzip(&document("", &root))
    }

    pub fn fake_save(dir: &Path, version: &str, data_version: i32) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("level.dat"), level_dat(version, data_version)).unwrap();
    }

    #[test]
    fn dimension_names() {
        assert_eq!(
            normalize_dimension("overworld").unwrap(),
            ("minecraft".into(), "overworld".into())
        );
        assert_eq!(
            normalize_dimension("minecraft:the_nether").unwrap().1,
            "the_nether"
        );
        assert_eq!(
            normalize_dimension("my_pack:sky").unwrap(),
            ("my_pack".into(), "sky".into())
        );
        assert!(normalize_dimension("").is_err());
        assert!(normalize_dimension("../etc").is_err());
        assert!(normalize_dimension("a:b:c").is_err());
        assert!(normalize_dimension("Over World").is_err());
    }

    #[test]
    fn finds_both_layouts() {
        let d = temp_dir("save-layouts");
        let old = d.join("old");
        fake_save(&old, "1.16.5", 2586);
        fs::create_dir_all(old.join("region")).unwrap();
        fs::create_dir_all(old.join("DIM-1/region")).unwrap();
        fs::write(old.join("region/r.0.0.mca"), b"").unwrap();
        fs::write(old.join("region/r.-1.2.mca"), vec![0u8; 8192]).unwrap();
        fs::write(old.join("region/notes.txt"), b"x").unwrap();
        let w = World::open(&old).unwrap();
        assert_eq!(w.level.version_name.as_deref(), Some("1.16.5"));
        assert_eq!(
            w.dimensions(),
            vec!["minecraft:overworld", "minecraft:the_nether"]
        );
        // an empty file is no region
        assert_eq!(w.regions("overworld").unwrap(), vec![(-1, 2)]);
        assert!(w.regions("the_end").is_err());
        let new = d.join("new");
        fake_save(&new, "26.3", 5023);
        fs::create_dir_all(new.join("dimensions/minecraft/overworld/region")).unwrap();
        fs::create_dir_all(new.join("dimensions/pack/sky/region")).unwrap();
        let w = World::open(&new).unwrap();
        assert_eq!(w.dimensions(), vec!["minecraft:overworld", "pack:sky"]);
        assert!(w.region_dir("pack:sky").is_ok());
        assert!(World::open(&d).is_err(), "no level.dat");
        fs::remove_dir_all(&d).unwrap();
    }
}
