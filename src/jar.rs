//! Reading single entries of the client jar.
//!
//! A client jar is a zip of 5,000 to 15,000 entries, most of them compiled classes. The central
//! directory is read once when the jar is opened; after that [`Jar::read`] seeks to one entry and
//! inflates only that. Nothing is ever extracted to disk.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Cursor, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use zip::ZipArchive;

/// The largest entry [`Jar::read`] will inflate. Nothing this crate reads comes near it (see the
/// two limits below); it only guards against a damaged or hostile archive.
pub const MAX_ENTRY: u64 = 16 * 1024 * 1024;

/// Parses JSON from bytes (no copy into a `String`), skipping a UTF-8 byte order mark
pub fn parse_json(bytes: &[u8]) -> Result<serde_json::Value> {
    let b = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    Ok(serde_json::from_slice(b)?)
}

/// The largest JSON or `.mcmeta` entry read: 128 KiB. Measured over the eight jars on the author's
/// machine, the largest blockstate, model, item definition, atlas, biome or `.mcmeta` is 24,324
/// bytes (`items/clock.json`, 26.3), so this is 5 times that. A JSON file is parsed into a tree,
/// which takes several times its size in memory, so the limit is kept low on purpose.
pub const MAX_JSON: u64 = 128 * 1024;

/// The largest PNG entry read: 8 MiB. The largest in the jars is 125 KB (a GUI texture).
pub const MAX_PNG: u64 = 8 * 1024 * 1024;

trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

pub struct Jar {
    zip: Mutex<ZipArchive<Box<dyn ReadSeek>>>,
    /// Entry name to index in the central directory
    index: HashMap<String, usize>,
    names: Vec<String>,
    path: Option<PathBuf>,
    skipped_names: usize,
}

fn is_unsafe_char(c: char) -> bool {
    c.is_control() || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// `s` with control and direction-override characters written out (`\u{1b}`), for printing text
/// that came from a jar (a name inside a JSON file, a version id) to a terminal
pub fn escape_control(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if is_unsafe_char(c) {
            out.push_str(&c.escape_unicode().to_string());
        } else {
            out.push(c);
        }
    }
    out
}

impl Jar {
    pub fn open(path: &Path) -> Result<Jar> {
        let f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut jar = Self::from_reader(Box::new(f))
            .with_context(|| format!("reading the zip directory of {}", path.display()))?;
        jar.path = Some(path.to_path_buf());
        Ok(jar)
    }

    /// A jar held in memory (tests use this with tiny synthetic jars)
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Jar> {
        Self::from_reader(Box::new(Cursor::new(bytes)))
    }

    fn from_reader(r: Box<dyn ReadSeek>) -> Result<Jar> {
        let zip = ZipArchive::new(r)?;
        let mut index = HashMap::with_capacity(zip.len());
        let mut names = Vec::with_capacity(zip.len());
        let mut skipped = 0;
        for (i, n) in zip.file_names().enumerate() {
            // A name with a control or direction-override character is never a game asset, and
            // printed to a terminal it could rewrite what the user reads: it is not indexed
            if n.chars().any(is_unsafe_char) {
                skipped += 1;
                continue;
            }
            index.insert(n.to_string(), i);
            names.push(n.to_string());
        }
        names.sort();
        names.dedup();
        Ok(Jar {
            zip: Mutex::new(zip),
            index,
            names,
            skipped_names: skipped,
            path: None,
        })
    }

    /// Entries left out of the index because their names hold control characters
    pub fn skipped_names(&self) -> usize {
        self.skipped_names
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Entry names in sorted order (directories end in `/`)
    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub fn contains(&self, name: &str) -> bool {
        self.index.contains_key(name)
    }

    /// File entries whose name starts with `prefix`, in sorted order
    pub fn names_under<'a>(&'a self, prefix: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        let start = self.names.partition_point(|n| n.as_str() < prefix);
        self.names[start..]
            .iter()
            .take_while(move |n| n.starts_with(prefix))
            .filter(|n| !n.ends_with('/'))
            .map(|s| s.as_str())
    }

    /// Inflates one entry, refusing more than [`MAX_ENTRY`] bytes
    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        self.read_limited(name, MAX_ENTRY)
    }

    /// Inflates one entry and fails if it is, or inflates to, more than `limit` bytes.
    ///
    /// The size a zip entry declares is not trusted: the entry is read through a limit, and the
    /// buffer is not sized from the header, so an entry that declares 100 bytes and inflates to
    /// hundreds of megabytes stops at `limit`.
    pub fn read_limited(&self, name: &str, limit: u64) -> Result<Vec<u8>> {
        let Some(&i) = self.index.get(name) else {
            bail!("{name}: not in the jar");
        };
        let mut z = self.zip.lock().unwrap();
        let f = z.by_index(i)?;
        if f.size() > limit {
            bail!(
                "{name}: declares {} bytes, over the {limit} byte limit",
                f.size()
            );
        }
        let mut out = Vec::new();
        f.take(limit + 1)
            .read_to_end(&mut out)
            .with_context(|| format!("inflating {name}"))?;
        if out.len() as u64 > limit {
            bail!("{name}: inflates to more than the {limit} byte limit (the zip header says otherwise)");
        }
        Ok(out)
    }

    /// A JSON or `.mcmeta` entry (at most [`MAX_JSON`] bytes), without a UTF-8 byte order mark
    pub fn read_json(&self, name: &str) -> Result<Vec<u8>> {
        let mut b = self.read_limited(name, MAX_JSON)?;
        if b.starts_with(&[0xEF, 0xBB, 0xBF]) {
            b.drain(..3);
        }
        Ok(b)
    }

    /// A PNG entry (at most [`MAX_PNG`] bytes)
    pub fn read_png(&self, name: &str) -> Result<Vec<u8>> {
        self.read_limited(name, MAX_PNG)
    }
}

#[cfg(test)]
mod tests {
    use super::testjar::JarBuilder;
    use super::*;

    /// A zip whose single deflated entry declares `claimed` bytes but inflates to `real`
    fn lying_jar(real: usize, claimed: u32) -> Jar {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            use std::io::Write;
            let mut w = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            w.start_file("big.json", opts).unwrap();
            w.write_all(&vec![b' '; real]).unwrap();
            w.finish().unwrap();
        }
        let mut bytes = buf.into_inner();
        // patch the uncompressed size in the local header (offset 22) and the central directory (24)
        let find = |sig: [u8; 4]| bytes.windows(4).position(|w| w == sig).unwrap();
        let local = find([0x50, 0x4b, 0x03, 0x04]);
        let central = find([0x50, 0x4b, 0x01, 0x02]);
        bytes[local + 22..local + 26].copy_from_slice(&claimed.to_le_bytes());
        bytes[central + 24..central + 28].copy_from_slice(&claimed.to_le_bytes());
        Jar::from_bytes(bytes).unwrap()
    }

    #[test]
    fn the_declared_size_is_not_trusted() {
        // 3 MiB of spaces deflates to a few kilobytes; the header says 100 bytes
        let jar = lying_jar(3 * 1024 * 1024, 100);
        let e = jar
            .read_limited("big.json", 64 * 1024)
            .unwrap_err()
            .to_string();
        assert!(e.contains("limit"), "{e}");
        assert!(jar.read_json("big.json").is_err());
    }

    #[test]
    fn an_honest_big_entry_is_refused_by_its_header() {
        let jar = JarBuilder::new().add("a.json", vec![b' '; 4096]).build();
        assert!(jar.read_limited("a.json", 1024).is_err());
        assert_eq!(jar.read_limited("a.json", 4096).unwrap().len(), 4096);
    }

    #[test]
    fn json_loses_its_byte_order_mark() {
        let jar = JarBuilder::new().add("a.json", b"\xEF\xBB\xBF{}").build();
        assert_eq!(jar.read_json("a.json").unwrap(), b"{}");
        assert_eq!(jar.names().iter().filter(|n| *n == "a.json").count(), 1);
        assert_eq!(jar.names_under("a").count(), 1);
    }
}

#[cfg(test)]
pub mod testjar {
    //! Builds tiny jars for the unit tests: no game data anywhere.
    use std::io::Write;

    use super::Jar;

    #[derive(Default)]
    pub struct JarBuilder {
        files: Vec<(String, Vec<u8>)>,
    }

    impl JarBuilder {
        pub fn new() -> Self {
            JarBuilder { files: Vec::new() }
        }
        pub fn add(mut self, name: &str, data: impl AsRef<[u8]>) -> Self {
            self.files.push((name.to_string(), data.as_ref().to_vec()));
            self
        }
        pub fn build_bytes(&self) -> Vec<u8> {
            let mut buf = std::io::Cursor::new(Vec::new());
            {
                let mut w = zip::ZipWriter::new(&mut buf);
                let opts = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored);
                for (n, d) in &self.files {
                    w.start_file(n.as_str(), opts).unwrap();
                    w.write_all(d).unwrap();
                }
                w.finish().unwrap();
            }
            buf.into_inner()
        }
        pub fn build(&self) -> Jar {
            Jar::from_bytes(self.build_bytes()).unwrap()
        }
    }

    /// A PNG of `w` x `h` made by `f(x, y) -> rgba`
    pub fn png(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&f(x, y));
            }
        }
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(&data).unwrap();
        }
        out
    }
}
