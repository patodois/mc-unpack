//! Region files (`r.<x>.<z>.mca`): 32 x 32 chunks in one file.
//!
//! ```text
//! 0      4096   1024 entries: sector offset (3 bytes, big endian) and sector count (1 byte); 0 = no chunk
//! 4096   4096   1024 timestamps (not read)
//! 8192   ...    sectors of 4096 bytes; a chunk is at offset * 4096:
//!                 length (4 bytes, big endian, counts the next byte too), compression (1 byte), data
//!               a compression byte with the high bit set (128 + id) means the data is not here but in
//!               `c.<chunk x>.<chunk z>.mcc` next to the region files (chunks over 1 MiB compressed)
//! ```
//!
//! The file is never loaded whole: the 4 KiB table is read when it is opened and a chunk is read by
//! seeking to it. Every offset and length in the file is checked against the file's size before it
//! is used, so a table that points anywhere is an error for that chunk, not a read of something else
//! or a large allocation.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::compress::{decompress, Compression};

pub const SECTOR: u64 = 4096;
/// The most a chunk may inflate to. A real chunk is 50 KiB to 2 MiB uncompressed.
pub const MAX_CHUNK_BYTES: usize = 24 << 20;
/// The most an external `.mcc` chunk may be compressed (the game's own limit for an in-file chunk
/// is 1 MiB; external ones exist because a chunk went over it)
pub const MAX_EXTERNAL_BYTES: u64 = 32 << 20;

pub struct Region {
    file: File,
    len: u64,
    entries: Vec<u32>,
    /// Where `r.x.z.mca` is, for the `.mcc` files beside it
    dir: PathBuf,
    /// The region's coordinates, from its file name
    pub x: i32,
    pub z: i32,
}

/// `r.-3.12.mca` to (-3, 12)
pub fn parse_region_name(name: &str) -> Option<(i32, i32)> {
    let rest = name.strip_prefix("r.")?.strip_suffix(".mca")?;
    let (x, z) = rest.split_once('.')?;
    // `-0` and `+1` are not what the game writes
    let ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 11
            && s.bytes()
                .enumerate()
                .all(|(i, b)| b.is_ascii_digit() || (i == 0 && b == b'-'))
    };
    if !ok(x) || !ok(z) || x == "-0" || z == "-0" {
        return None;
    }
    Some((x.parse().ok()?, z.parse().ok()?))
}

impl Region {
    pub fn open(path: &Path) -> Result<Region> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .context("region path has no file name")?;
        let (x, z) = parse_region_name(name)
            .with_context(|| format!("{name:?} is not named r.<x>.<z>.mca"))?;
        let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let len = file.metadata()?.len();
        let mut entries = vec![0u32; 1024];
        if len != 0 {
            if len < 2 * SECTOR {
                bail!("{name}: {len} bytes is shorter than the 8192-byte header");
            }
            let mut raw = vec![0u8; SECTOR as usize];
            file.read_exact(&mut raw)
                .with_context(|| format!("reading the table of {name}"))?;
            for (e, c) in entries.iter_mut().zip(raw.chunks_exact(4)) {
                *e = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
            }
        }
        Ok(Region {
            file,
            len,
            entries,
            dir: path.parent().map(Path::to_path_buf).unwrap_or_default(),
            x,
            z,
        })
    }

    fn entry(lx: u32, lz: u32) -> usize {
        (lx + lz * 32) as usize
    }

    /// Whether the table lists a chunk at the local coordinates 0 to 31 (it may still be damaged)
    pub fn has_chunk(&self, lx: u32, lz: u32) -> bool {
        lx < 32 && lz < 32 && self.entries[Self::entry(lx, lz)] != 0
    }

    pub fn chunk_count(&self) -> usize {
        self.entries.iter().filter(|&&e| e != 0).count()
    }

    /// The decompressed NBT of the chunk at local (lx, lz), or `None` when the table has none
    pub fn read_chunk(&mut self, lx: u32, lz: u32) -> Result<Option<Vec<u8>>> {
        if lx >= 32 || lz >= 32 {
            bail!("chunk ({lx}, {lz}) is outside the 32 x 32 of a region");
        }
        let e = self.entries[Self::entry(lx, lz)];
        if e == 0 {
            return Ok(None);
        }
        let (cx, cz) = (
            self.x as i64 * 32 + lx as i64,
            self.z as i64 * 32 + lz as i64,
        );
        let ctx = || {
            format!(
                "chunk {cx},{cz} (region {},{} local {lx},{lz})",
                self.x, self.z
            )
        };
        let sector = (e >> 8) as u64;
        let count = (e & 0xFF) as u64;
        if sector < 2 {
            bail!(
                "{}: its table entry puts it at sector {sector}, inside the header",
                ctx()
            );
        }
        if count == 0 {
            bail!("{}: its table entry has an offset and no sectors", ctx());
        }
        let at = sector * SECTOR;
        if at + 5 > self.len {
            bail!("{}: starts at byte {at} of a {}-byte file", ctx(), self.len);
        }
        self.file.seek(SeekFrom::Start(at)).with_context(ctx)?;
        let mut head = [0u8; 5];
        self.file.read_exact(&mut head).with_context(ctx)?;
        let length = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as u64;
        let mut comp = head[4];
        if length == 0 {
            bail!("{}: its length is 0", ctx());
        }
        // the length counts the compression byte; the data must stay inside the sectors it was given
        // and inside the file
        if length + 4 > count * SECTOR {
            bail!(
                "{}: length {length} does not fit in its {count} sector(s)",
                ctx()
            );
        }
        let data_len = length - 1;
        if at + 5 + data_len > self.len {
            bail!("{}: runs past the end of the file", ctx());
        }
        let external = comp & 0x80 != 0;
        comp &= 0x7F;
        let kind = Compression::from_id(comp).with_context(ctx)?;
        let payload = if external {
            let name = format!("c.{cx}.{cz}.mcc");
            let p = self.dir.join(&name);
            let f = File::open(&p).with_context(|| format!("{}: external chunk {name}", ctx()))?;
            let mut buf = Vec::new();
            f.take(MAX_EXTERNAL_BYTES + 1)
                .read_to_end(&mut buf)
                .with_context(|| format!("reading {name}"))?;
            if buf.len() as u64 > MAX_EXTERNAL_BYTES {
                bail!("{}: {name} is over {MAX_EXTERNAL_BYTES} bytes", ctx());
            }
            buf
        } else {
            let mut buf = vec![0u8; data_len as usize];
            self.file.read_exact(&mut buf).with_context(ctx)?;
            buf
        };
        decompress(kind, &payload, MAX_CHUNK_BYTES)
            .with_context(|| format!("{} ({} data)", ctx(), kind.name()))
            .map(Some)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::world::compress::tests::{gzip, lz4_stored, zlib};

    /// Builds the bytes of a region file from (local x, local z, compression id, payload)
    pub fn region_bytes(chunks: &[(u32, u32, u8, Vec<u8>)]) -> Vec<u8> {
        let mut table = vec![0u8; 8192];
        let mut body: Vec<u8> = Vec::new();
        let mut next_sector = 2u32;
        for (lx, lz, comp, payload) in chunks {
            let mut rec = Vec::new();
            rec.extend_from_slice(&(payload.len() as u32 + 1).to_be_bytes());
            rec.push(*comp);
            rec.extend_from_slice(payload);
            let sectors = (rec.len() as u32).div_ceil(4096);
            rec.resize((sectors * 4096) as usize, 0);
            let i = (lx + lz * 32) as usize * 4;
            let e = (next_sector << 8) | sectors;
            table[i..i + 4].copy_from_slice(&e.to_be_bytes());
            body.extend_from_slice(&rec);
            next_sector += sectors;
        }
        table.extend_from_slice(&body);
        table
    }

    pub fn temp_dir(tag: &str) -> PathBuf {
        // tests run side by side: each folder is its own
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("mc-unpack-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn names() {
        assert_eq!(parse_region_name("r.0.0.mca"), Some((0, 0)));
        assert_eq!(parse_region_name("r.-3.12.mca"), Some((-3, 12)));
        assert_eq!(parse_region_name("r.-0.1.mca"), None);
        assert_eq!(parse_region_name("r.+1.1.mca"), None);
        assert_eq!(parse_region_name("r.1.mca"), None);
        assert_eq!(parse_region_name("r.1.2.mcr"), None);
        assert_eq!(parse_region_name("r.99999999999999.2.mca"), None);
        assert_eq!(parse_region_name("r..2.mca"), None);
        assert_eq!(parse_region_name("x.1.2.mca"), None);
    }

    #[test]
    fn reads_every_compression_and_absent_chunks() {
        let dir = temp_dir("region-comp");
        let data = b"some nbt bytes".to_vec();
        let bytes = region_bytes(&[
            (0, 0, 2, zlib(&data)),
            (1, 0, 1, gzip(&data)),
            (2, 0, 3, data.clone()),
            (3, 0, 4, lz4_stored(&data)),
        ]);
        let p = dir.join("r.2.-1.mca");
        std::fs::write(&p, bytes).unwrap();
        let mut r = Region::open(&p).unwrap();
        assert_eq!((r.x, r.z), (2, -1));
        assert_eq!(r.chunk_count(), 4);
        for lx in 0..4 {
            assert_eq!(r.read_chunk(lx, 0).unwrap().unwrap(), data, "chunk {lx}");
        }
        assert!(r.read_chunk(4, 0).unwrap().is_none());
        assert!(!r.has_chunk(4, 0) && r.has_chunk(3, 0));
        assert!(r.read_chunk(32, 0).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn external_chunks_come_from_the_mcc_file() {
        let dir = temp_dir("region-mcc");
        let data = b"a very large chunk".to_vec();
        // region 1,1: local (2,3) is chunk (34, 35)
        let bytes = region_bytes(&[(2, 3, 2 | 0x80, Vec::new())]);
        std::fs::write(dir.join("r.1.1.mca"), bytes).unwrap();
        let mut r = Region::open(&dir.join("r.1.1.mca")).unwrap();
        // no .mcc yet
        assert!(format!("{:#}", r.read_chunk(2, 3).unwrap_err()).contains("c.34.35.mcc"));
        std::fs::write(dir.join("c.34.35.mcc"), zlib(&data)).unwrap();
        assert_eq!(r.read_chunk(2, 3).unwrap().unwrap(), data);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_empty_file_is_an_empty_region_and_a_short_one_is_an_error() {
        let dir = temp_dir("region-empty");
        std::fs::write(dir.join("r.0.0.mca"), b"").unwrap();
        let mut r = Region::open(&dir.join("r.0.0.mca")).unwrap();
        assert_eq!(r.chunk_count(), 0);
        assert!(r.read_chunk(0, 0).unwrap().is_none());
        std::fs::write(dir.join("r.0.1.mca"), vec![0u8; 5000]).unwrap();
        assert!(Region::open(&dir.join("r.0.1.mca")).is_err());
        std::fs::write(dir.join("bad.mca"), vec![0u8; 9000]).unwrap();
        assert!(Region::open(&dir.join("bad.mca")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_table_that_lies_is_an_error_per_chunk() {
        let dir = temp_dir("region-lies");
        let good = region_bytes(&[(0, 0, 2, zlib(b"ok"))]);
        let p = dir.join("r.0.0.mca");
        let try_entry = |entry: u32, tweak: &dyn Fn(&mut Vec<u8>)| -> String {
            let mut b = good.clone();
            b[0..4].copy_from_slice(&entry.to_be_bytes());
            tweak(&mut b);
            std::fs::write(&p, &b).unwrap();
            let mut r = Region::open(&p).unwrap();
            format!("{:#}", r.read_chunk(0, 0).unwrap_err())
        };
        // inside the header
        assert!(try_entry(1 << 8 | 1, &|_| {}).contains("inside the header"));
        // offset with no sectors
        assert!(try_entry(2 << 8, &|_| {}).contains("no sectors"));
        // past the end of the file
        assert!(try_entry(0xFFFFFF << 8 | 1, &|_| {}).contains("starts at byte"));
        // a length that does not fit its sectors
        assert!(try_entry(2 << 8 | 1, &|b| b[8192..8196]
            .copy_from_slice(&5000u32.to_be_bytes()))
        .contains("does not fit"));
        // a length of 0
        assert!(try_entry(2 << 8 | 1, &|b| b[8192..8196]
            .copy_from_slice(&0u32.to_be_bytes()))
        .contains("length is 0"));
        // a file cut inside the chunk
        assert!(try_entry(2 << 8 | 1, &|b| b.truncate(8192 + 8)).contains("past the end"));
        // an unknown compression
        assert!(try_entry(2 << 8 | 1, &|b| b[8196] = 9).contains("unknown compression"));
        // data that is not what the compression says
        assert!(try_entry(2 << 8 | 1, &|b| b[8197] ^= 0xFF).contains("zlib"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
