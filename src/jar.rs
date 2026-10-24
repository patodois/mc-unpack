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

/// The largest entry [`Jar::read`] will inflate. The biggest asset in the jars checked is a few
/// hundred kilobytes; this only guards against a damaged or hostile archive.
pub const MAX_ENTRY: u64 = 256 * 1024 * 1024;

trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

pub struct Jar {
    zip: Mutex<ZipArchive<Box<dyn ReadSeek>>>,
    /// Entry name to index in the central directory
    index: HashMap<String, usize>,
    names: Vec<String>,
    path: Option<PathBuf>,
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
        for (i, n) in zip.file_names().enumerate() {
            index.insert(n.to_string(), i);
            names.push(n.to_string());
        }
        names.sort();
        Ok(Jar {
            zip: Mutex::new(zip),
            index,
            names,
            path: None,
        })
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

    /// Inflates one entry
    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        let Some(&i) = self.index.get(name) else {
            bail!("{name}: not in the jar");
        };
        let mut z = self.zip.lock().unwrap();
        let mut f = z.by_index(i)?;
        if f.size() > MAX_ENTRY {
            bail!("{name}: {} bytes is over the {MAX_ENTRY} byte limit", f.size());
        }
        let mut out = Vec::with_capacity(f.size() as usize);
        f.read_to_end(&mut out)
            .with_context(|| format!("inflating {name}"))?;
        Ok(out)
    }

    pub fn read_string(&self, name: &str) -> Result<String> {
        let b = self.read(name)?;
        // Mojang's JSON is UTF-8; some older files carry a byte order mark
        let s = String::from_utf8(b).with_context(|| format!("{name}: not UTF-8"))?;
        Ok(s.trim_start_matches('\u{feff}').to_string())
    }
}

#[cfg(test)]
pub mod testjar {
    //! Builds tiny jars for the unit tests: no game data anywhere.
    use std::io::Write;

    use super::Jar;

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
