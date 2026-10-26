//! NBT, the tag format of `level.dat` and of every chunk: big-endian, named tags, no schema.
//!
//! Written from the format as the game's files use it, and bounded: this reads files that live in a
//! folder the user may have got from anywhere. Nothing is sized from a length the file declares
//! until the bytes for it are known to be there, nesting and the number of tags are capped, and a
//! tag the format does not allow is an error rather than a guess. Nothing here panics on any input
//! (the fuzz test in `world/mod.rs` feeds it mutations of real and synthetic files).

use anyhow::{bail, Result};

/// How deep compounds and lists may nest (the game allows 512; the deepest thing in a real chunk is
/// a dozen levels, and a shulker box inside a shulker box adds about five per level)
pub const MAX_DEPTH: usize = 128;

/// How many tags one document may hold. A real chunk is a few thousand; a full one with every
/// block entity, a few tens of thousands.
pub const MAX_TAGS: usize = 2_000_000;

#[derive(Clone, Debug, PartialEq)]
pub enum Tag {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<i8>),
    String(String),
    List(Vec<Tag>),
    /// Keys in file order; a key twice keeps both, and [`Tag::get`] finds the last (the game's
    /// compound is a map, where the later one replaces the earlier)
    Compound(Vec<(String, Tag)>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl Tag {
    pub fn type_id(&self) -> u8 {
        match self {
            Tag::Byte(_) => 1,
            Tag::Short(_) => 2,
            Tag::Int(_) => 3,
            Tag::Long(_) => 4,
            Tag::Float(_) => 5,
            Tag::Double(_) => 6,
            Tag::ByteArray(_) => 7,
            Tag::String(_) => 8,
            Tag::List(_) => 9,
            Tag::Compound(_) => 10,
            Tag::IntArray(_) => 11,
            Tag::LongArray(_) => 12,
        }
    }

    /// The value of a key of a compound (the last one when it is written twice)
    pub fn get(&self, key: &str) -> Option<&Tag> {
        match self {
            Tag::Compound(v) => v.iter().rev().find(|(k, _)| k == key).map(|(_, t)| t),
            _ => None,
        }
    }

    /// Any whole-number tag as an `i64` (the game reads a byte, short, int or long where it wants a
    /// number and the file may have either)
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Tag::Byte(v) => Some(*v as i64),
            Tag::Short(v) => Some(*v as i64),
            Tag::Int(v) => Some(*v as i64),
            Tag::Long(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Tag::Float(v) => Some(*v as f64),
            Tag::Double(v) => Some(*v),
            other => other.as_i64().map(|v| v as f64),
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Tag::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Tag]> {
        match self {
            Tag::List(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_compound(&self) -> Option<&[(String, Tag)]> {
        match self {
            Tag::Compound(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_long_array(&self) -> Option<&[i64]> {
        match self {
            Tag::LongArray(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_int_array(&self) -> Option<&[i32]> {
        match self {
            Tag::IntArray(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_byte_array(&self) -> Option<&[i8]> {
        match self {
            Tag::ByteArray(v) => Some(v),
            _ => None,
        }
    }

    /// A short, bounded rendering for the `world-nbt` command: arrays are summarized, lists and
    /// compounds are cut at `depth`
    pub fn render(&self, depth: usize) -> String {
        fn clean(s: &str) -> String {
            s.chars()
                .map(|c| if c.is_control() { '?' } else { c })
                .collect()
        }
        match self {
            Tag::Byte(v) => format!("{v}b"),
            Tag::Short(v) => format!("{v}s"),
            Tag::Int(v) => format!("{v}"),
            Tag::Long(v) => format!("{v}L"),
            Tag::Float(v) => format!("{v}f"),
            Tag::Double(v) => format!("{v}d"),
            Tag::ByteArray(v) => format!("[B; {} bytes]", v.len()),
            Tag::IntArray(v) => {
                if v.len() <= 8 {
                    format!("{v:?}")
                } else {
                    format!("[I; {} ints]", v.len())
                }
            }
            Tag::LongArray(v) => format!("[L; {} longs]", v.len()),
            Tag::String(s) => format!("{:?}", clean(s)),
            Tag::List(v) => {
                if depth == 0 {
                    return format!("[{} items]", v.len());
                }
                let shown: Vec<String> = v.iter().take(6).map(|t| t.render(depth - 1)).collect();
                let more = if v.len() > 6 {
                    format!(", ... {} more", v.len() - 6)
                } else {
                    String::new()
                };
                format!("[{}{more}]", shown.join(", "))
            }
            Tag::Compound(v) => {
                if depth == 0 {
                    return format!("{{{} keys}}", v.len());
                }
                let shown: Vec<String> = v
                    .iter()
                    .take(40)
                    .map(|(k, t)| format!("{}: {}", clean(k), t.render(depth - 1)))
                    .collect();
                let more = if v.len() > 40 {
                    format!(", ... {} more", v.len() - 40)
                } else {
                    String::new()
                };
                format!("{{{}{more}}}", shown.join(", "))
            }
        }
    }
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
    tags_left: usize,
}

impl<'a> Reader<'a> {
    fn remaining(&self) -> usize {
        self.b.len() - self.pos
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.remaining() {
            bail!(
                "NBT ends early: {n} bytes wanted at {}, {} left",
                self.pos,
                self.remaining()
            );
        }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn i16(&mut self) -> Result<i16> {
        let s = self.take(2)?;
        Ok(i16::from_be_bytes([s[0], s[1]]))
    }

    fn i32(&mut self) -> Result<i32> {
        let s = self.take(4)?;
        Ok(i32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }

    fn i64(&mut self) -> Result<i64> {
        let s = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        Ok(i64::from_be_bytes(a))
    }

    fn string(&mut self) -> Result<String> {
        let n = self.i16()? as u16 as usize;
        let s = self.take(n)?;
        Ok(decode_mutf8(s))
    }

    /// A length of `n` elements of `size` bytes, checked against what is left before anything is
    /// allocated from it
    fn array_len(&mut self, size: usize) -> Result<usize> {
        let n = self.i32()?;
        if n < 0 {
            bail!("NBT array with a negative length {n}");
        }
        let n = n as usize;
        match n.checked_mul(size) {
            Some(bytes) if bytes <= self.remaining() => Ok(n),
            _ => bail!(
                "NBT array of {n} elements does not fit in the {} bytes left",
                self.remaining()
            ),
        }
    }

    fn count(&mut self) -> Result<()> {
        if self.tags_left == 0 {
            bail!("NBT holds more than {MAX_TAGS} tags");
        }
        self.tags_left -= 1;
        Ok(())
    }

    fn payload(&mut self, ty: u8, depth: usize) -> Result<Tag> {
        self.count()?;
        Ok(match ty {
            1 => Tag::Byte(self.u8()? as i8),
            2 => Tag::Short(self.i16()?),
            3 => Tag::Int(self.i32()?),
            4 => Tag::Long(self.i64()?),
            5 => Tag::Float(f32::from_bits(self.i32()? as u32)),
            6 => Tag::Double(f64::from_bits(self.i64()? as u64)),
            7 => {
                let n = self.array_len(1)?;
                Tag::ByteArray(self.take(n)?.iter().map(|&b| b as i8).collect())
            }
            8 => Tag::String(self.string()?),
            9 => {
                if depth >= MAX_DEPTH {
                    bail!("NBT nests deeper than {MAX_DEPTH}");
                }
                let et = self.u8()?;
                let n = self.i32()?;
                if n < 0 {
                    bail!("NBT list with a negative length {n}");
                }
                let n = n as usize;
                if et == 0 {
                    if n != 0 {
                        bail!("NBT list of {n} End tags");
                    }
                    return Ok(Tag::List(Vec::new()));
                }
                if et > 12 {
                    bail!("NBT list of unknown type {et}");
                }
                // the smallest element is a byte; a list longer than the bytes left cannot be real
                if n > self.remaining() {
                    bail!(
                        "NBT list of {n} items does not fit in the {} bytes left",
                        self.remaining()
                    );
                }
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.payload(et, depth + 1)?);
                }
                Tag::List(v)
            }
            10 => {
                if depth >= MAX_DEPTH {
                    bail!("NBT nests deeper than {MAX_DEPTH}");
                }
                let mut v = Vec::new();
                loop {
                    let t = self.u8()?;
                    if t == 0 {
                        break;
                    }
                    if t > 12 {
                        bail!("NBT tag of unknown type {t}");
                    }
                    let k = self.string()?;
                    v.push((k, self.payload(t, depth + 1)?));
                }
                Tag::Compound(v)
            }
            11 => {
                let n = self.array_len(4)?;
                let raw = self.take(n * 4)?;
                Tag::IntArray(
                    raw.chunks_exact(4)
                        .map(|c| i32::from_be_bytes([c[0], c[1], c[2], c[3]]))
                        .collect(),
                )
            }
            12 => {
                let n = self.array_len(8)?;
                let raw = self.take(n * 8)?;
                Tag::LongArray(
                    raw.chunks_exact(8)
                        .map(|c| {
                            let mut a = [0u8; 8];
                            a.copy_from_slice(c);
                            i64::from_be_bytes(a)
                        })
                        .collect(),
                )
            }
            other => bail!("NBT tag of unknown type {other}"),
        })
    }
}

/// Java's "modified UTF-8" (NUL as C0 80, supplementary characters as surrogate pairs). Text that is
/// plain UTF-8 reads as itself; anything else is decoded with replacement characters, which only
/// matters for text the game would not write into a block name anyway.
fn decode_mutf8(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => {
            // C0 80 is NUL; everything else falls through lossy
            let mut fixed = Vec::with_capacity(b.len());
            let mut i = 0;
            while i < b.len() {
                if b[i] == 0xC0 && b.get(i + 1) == Some(&0x80) {
                    fixed.push(0);
                    i += 2;
                } else {
                    fixed.push(b[i]);
                    i += 1;
                }
            }
            String::from_utf8_lossy(&fixed).into_owned()
        }
    }
}

/// Parses a document: one named tag, as `level.dat` and a chunk are. Returns the name and the tag.
/// Bytes after the root are an error (a real file has none).
pub fn parse(data: &[u8]) -> Result<(String, Tag)> {
    let mut r = Reader {
        b: data,
        pos: 0,
        tags_left: MAX_TAGS,
    };
    let ty = r.u8()?;
    if ty == 0 {
        bail!("NBT document starts with an End tag");
    }
    if ty > 12 {
        bail!("NBT document starts with an unknown tag type {ty}");
    }
    let name = r.string()?;
    let tag = r.payload(ty, 0)?;
    if r.remaining() != 0 {
        bail!("{} bytes follow the NBT root", r.remaining());
    }
    Ok((name, tag))
}

/// Builders for tests and for synthetic worlds: the writer side of the format
pub mod write {
    use super::Tag;

    fn put_str(out: &mut Vec<u8>, s: &str) {
        out.extend_from_slice(&(s.len() as u16).to_be_bytes());
        out.extend_from_slice(s.as_bytes());
    }

    fn put_payload(out: &mut Vec<u8>, t: &Tag) {
        match t {
            Tag::Byte(v) => out.push(*v as u8),
            Tag::Short(v) => out.extend_from_slice(&v.to_be_bytes()),
            Tag::Int(v) => out.extend_from_slice(&v.to_be_bytes()),
            Tag::Long(v) => out.extend_from_slice(&v.to_be_bytes()),
            Tag::Float(v) => out.extend_from_slice(&v.to_bits().to_be_bytes()),
            Tag::Double(v) => out.extend_from_slice(&v.to_bits().to_be_bytes()),
            Tag::ByteArray(v) => {
                out.extend_from_slice(&(v.len() as i32).to_be_bytes());
                out.extend(v.iter().map(|&b| b as u8));
            }
            Tag::String(s) => put_str(out, s),
            Tag::List(v) => {
                out.push(v.first().map_or(0, Tag::type_id));
                out.extend_from_slice(&(v.len() as i32).to_be_bytes());
                for t in v {
                    put_payload(out, t);
                }
            }
            Tag::Compound(v) => {
                for (k, t) in v {
                    out.push(t.type_id());
                    put_str(out, k);
                    put_payload(out, t);
                }
                out.push(0);
            }
            Tag::IntArray(v) => {
                out.extend_from_slice(&(v.len() as i32).to_be_bytes());
                for x in v {
                    out.extend_from_slice(&x.to_be_bytes());
                }
            }
            Tag::LongArray(v) => {
                out.extend_from_slice(&(v.len() as i32).to_be_bytes());
                for x in v {
                    out.extend_from_slice(&x.to_be_bytes());
                }
            }
        }
    }

    /// A document with the root named `name`
    pub fn document(name: &str, root: &Tag) -> Vec<u8> {
        let mut out = vec![root.type_id()];
        put_str(&mut out, name);
        put_payload(&mut out, root);
        out
    }

    pub fn compound(items: Vec<(&str, Tag)>) -> Tag {
        Tag::Compound(items.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    pub fn string(s: &str) -> Tag {
        Tag::String(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::write::*;
    use super::*;

    fn sample() -> Tag {
        compound(vec![
            ("b", Tag::Byte(-3)),
            ("s", Tag::Short(300)),
            ("i", Tag::Int(-70000)),
            ("l", Tag::Long(1 << 40)),
            ("f", Tag::Float(1.5)),
            ("d", Tag::Double(-2.25)),
            ("ba", Tag::ByteArray(vec![1, -2, 3])),
            ("str", string("minecraft:stone")),
            ("list", Tag::List(vec![Tag::Int(1), Tag::Int(2)])),
            ("empty", Tag::List(vec![])),
            ("c", compound(vec![("x", Tag::Int(7))])),
            ("ia", Tag::IntArray(vec![1, -1])),
            ("la", Tag::LongArray(vec![i64::MIN, 5])),
        ])
    }

    #[test]
    fn round_trips_every_type() {
        let bytes = document("root", &sample());
        let (name, tag) = parse(&bytes).unwrap();
        assert_eq!(name, "root");
        assert_eq!(tag, sample());
        assert_eq!(tag.get("i").unwrap().as_i64(), Some(-70000));
        assert_eq!(tag.get("c").unwrap().get("x").unwrap().as_i64(), Some(7));
        assert_eq!(tag.get("f").unwrap().as_f64(), Some(1.5));
        assert_eq!(tag.get("la").unwrap().as_long_array().unwrap()[1], 5);
        assert!(tag.get("missing").is_none());
    }

    #[test]
    fn a_key_twice_reads_the_later_one() {
        let t = Tag::Compound(vec![("a".into(), Tag::Int(1)), ("a".into(), Tag::Int(2))]);
        let (_, back) = parse(&document("", &t)).unwrap();
        assert_eq!(back.get("a").unwrap().as_i64(), Some(2));
    }

    #[test]
    fn every_truncation_is_an_error_not_a_panic() {
        let bytes = document("root", &sample());
        for n in 0..bytes.len() {
            assert!(parse(&bytes[..n]).is_err(), "prefix of {n} bytes");
        }
    }

    #[test]
    fn lengths_larger_than_the_file_are_refused_before_allocating() {
        // a byte array that claims 2 GB
        let mut b = vec![7u8, 0, 0];
        b.extend_from_slice(&i32::MAX.to_be_bytes());
        b.extend_from_slice(&[0; 4]);
        assert!(parse(&b).unwrap_err().to_string().contains("does not fit"));
        // a long array that claims more than usize can multiply
        let mut b = vec![12u8, 0, 0];
        b.extend_from_slice(&i32::MAX.to_be_bytes());
        assert!(parse(&b).is_err());
        // negative
        let mut b = vec![11u8, 0, 0];
        b.extend_from_slice(&(-1i32).to_be_bytes());
        assert!(parse(&b).unwrap_err().to_string().contains("negative"));
        // a list that claims a million items
        let mut b = vec![9u8, 0, 0, 3];
        b.extend_from_slice(&1_000_000i32.to_be_bytes());
        assert!(parse(&b).is_err());
    }

    #[test]
    fn nesting_is_capped() {
        // lists of lists of lists ...
        let mut b = vec![9u8, 0, 0];
        for _ in 0..(MAX_DEPTH + 10) {
            b.extend_from_slice(&[9, 0, 0, 0, 1]);
        }
        let err = parse(&b).unwrap_err().to_string();
        assert!(
            err.contains("deeper") || err.contains("ends early"),
            "{err}"
        );
        // and compounds
        let mut b = vec![10u8, 0, 0];
        for _ in 0..(MAX_DEPTH + 10) {
            b.extend_from_slice(&[10, 0, 1, b'a']);
        }
        assert!(parse(&b).is_err());
    }

    #[test]
    fn bad_types_and_trailing_bytes_are_errors() {
        assert!(parse(&[0]).is_err());
        assert!(parse(&[13, 0, 0]).is_err());
        let mut ok = document("", &Tag::Int(1));
        ok.push(0);
        assert!(parse(&ok).unwrap_err().to_string().contains("follow"));
        // a list of End tags with a length
        let b = [9u8, 0, 0, 0, 0, 0, 0, 2];
        assert!(parse(&b).is_err());
        // a list of unknown type
        let b = [9u8, 0, 0, 99, 0, 0, 0, 0];
        assert!(parse(&b).is_err());
    }

    #[test]
    fn text_survives_nul_and_non_utf8() {
        // modified UTF-8 NUL
        let mut b = vec![8u8, 0, 0, 0, 3, b'a', 0xC0, 0x80];
        assert_eq!(parse(&b).unwrap().1.as_str(), Some("a\0"));
        b[5] = 0xFF;
        assert!(parse(&b).unwrap().1.as_str().is_some());
    }

    #[test]
    fn rendering_is_bounded_and_clean() {
        let t = compound(vec![
            ("k\n", string("a\u{7}b")),
            ("big", Tag::LongArray(vec![0; 9999])),
        ]);
        let s = t.render(3);
        assert!(!s.contains('\n') && !s.contains('\u{7}'));
        assert!(s.contains("9999 longs"));
    }
}
