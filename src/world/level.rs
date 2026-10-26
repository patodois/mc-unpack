//! `level.dat`: a gzip-compressed NBT file at the root of a save. Only what picks the jar and the
//! spawn point is read.

use std::io::Read;
use std::path::Path;

use anyhow::{bail, Context, Result};

use super::compress::{decompress, Compression};
use super::nbt::{self, Tag};
use super::region::open_regular;

/// A real `level.dat` is a few KiB (a Forge world's registry list makes it 60 KiB)
pub const MAX_LEVEL_DAT: u64 = 16 << 20;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LevelInfo {
    /// The save's `Data.DataVersion`: the number the game gives the format of its data. Absent in
    /// saves from before 1.9 (15w32a).
    pub data_version: Option<i64>,
    /// `Data.Version.Name`, `26.3`, `1.12.2` ... (absent before 1.9)
    pub version_name: Option<String>,
    pub snapshot: Option<bool>,
    pub level_name: Option<String>,
    /// The world spawn: `Data.SpawnX/Y/Z`, or `Data.spawn.pos` in the versions that moved it
    pub spawn: Option<[i32; 3]>,
    /// The dimension the spawn is in, where the save says (26.x)
    pub spawn_dimension: Option<String>,
}

pub fn parse_level(root: &Tag) -> Result<LevelInfo> {
    let data = root
        .get("Data")
        .context("level.dat has no `Data` compound")?;
    if data.as_compound().is_none() {
        bail!("level.dat `Data` is not a compound");
    }
    let mut info = LevelInfo {
        data_version: data.get("DataVersion").and_then(Tag::as_i64),
        ..LevelInfo::default()
    };
    if let Some(v) = data.get("Version") {
        info.version_name = v.get("Name").and_then(Tag::as_str).map(String::from);
        info.snapshot = v.get("Snapshot").and_then(Tag::as_i64).map(|b| b != 0);
        if info.data_version.is_none() {
            info.data_version = v.get("Id").and_then(Tag::as_i64);
        }
    }
    info.level_name = data
        .get("LevelName")
        .and_then(Tag::as_str)
        .map(String::from);
    let int = |k: &str| {
        data.get(k)
            .and_then(Tag::as_i64)
            .and_then(|v| i32::try_from(v).ok())
    };
    if let (Some(x), Some(y), Some(z)) = (int("SpawnX"), int("SpawnY"), int("SpawnZ")) {
        info.spawn = Some([x, y, z]);
    } else if let Some(sp) = data.get("spawn") {
        if let Some(p) = sp.get("pos").and_then(Tag::as_int_array) {
            if p.len() == 3 {
                info.spawn = Some([p[0], p[1], p[2]]);
            }
        }
        info.spawn_dimension = sp.get("dimension").and_then(Tag::as_str).map(String::from);
    }
    Ok(info)
}

pub fn read_level_dat(path: &Path) -> Result<LevelInfo> {
    let f = open_regular(path)?;
    let mut raw = Vec::new();
    f.take(MAX_LEVEL_DAT + 1).read_to_end(&mut raw)?;
    if raw.len() as u64 > MAX_LEVEL_DAT {
        bail!("{} is over {MAX_LEVEL_DAT} bytes", path.display());
    }
    // the file is gzip; an uncompressed one is accepted as some tools write it
    let bytes = if raw.starts_with(&[0x1f, 0x8b]) {
        decompress(Compression::Gzip, &raw, 64 << 20)
            .with_context(|| format!("reading {}", path.display()))?
    } else {
        raw
    };
    let (_, root) = nbt::parse(&bytes).with_context(|| format!("parsing {}", path.display()))?;
    parse_level(&root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::nbt::write::*;

    #[test]
    fn reads_the_old_and_the_new_spawn() {
        let old = compound(vec![(
            "Data",
            compound(vec![
                ("DataVersion", Tag::Int(1343)),
                (
                    "Version",
                    compound(vec![("Name", string("1.12.2")), ("Snapshot", Tag::Byte(0))]),
                ),
                ("SpawnX", Tag::Int(-70)),
                ("SpawnY", Tag::Int(4)),
                ("SpawnZ", Tag::Int(1143)),
                ("LevelName", string("New World")),
            ]),
        )]);
        let i = parse_level(&old).unwrap();
        assert_eq!(i.data_version, Some(1343));
        assert_eq!(i.version_name.as_deref(), Some("1.12.2"));
        assert_eq!(i.spawn, Some([-70, 4, 1143]));
        assert_eq!(i.snapshot, Some(false));
        let new = compound(vec![(
            "Data",
            compound(vec![
                ("DataVersion", Tag::Int(5023)),
                (
                    "spawn",
                    compound(vec![
                        ("pos", Tag::IntArray(vec![0, -60, 0])),
                        ("dimension", string("minecraft:overworld")),
                    ]),
                ),
            ]),
        )]);
        let i = parse_level(&new).unwrap();
        assert_eq!(i.spawn, Some([0, -60, 0]));
        assert_eq!(i.spawn_dimension.as_deref(), Some("minecraft:overworld"));
        // nothing at all is fine; no Data is not
        assert_eq!(
            parse_level(&compound(vec![("Data", compound(vec![]))])).unwrap(),
            LevelInfo::default()
        );
        assert!(parse_level(&compound(vec![])).is_err());
        assert!(parse_level(&compound(vec![("Data", Tag::Int(1))])).is_err());
    }
}
