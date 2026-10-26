//! The compressions a region file chunk can have, all bounded by an output limit.
//!
//! | id | name | notes |
//! |----|------|-------|
//! | 1 | gzip | `level.dat`, and chunks of very old worlds |
//! | 2 | zlib | the default of every version that has region files |
//! | 3 | none | 1.15.1 and later accept it |
//! | 4 | LZ4 | 1.20.5 and later, when the server asks for it. The framing is the one the game's own library writes (lz4-java's block stream: `LZ4Block`, a token, the lengths, an xxHash32 of the block, the block), read here from that format's description |
//! | 127 | custom | a name follows; not read |
//!
//! No input can make any of them produce more than the limit it is given.

use std::io::Read;

use anyhow::{bail, Context, Result};
use flate2::read::{GzDecoder, ZlibDecoder};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    Gzip,
    Zlib,
    None,
    Lz4,
}

impl Compression {
    pub fn from_id(id: u8) -> Result<Compression> {
        match id {
            1 => Ok(Compression::Gzip),
            2 => Ok(Compression::Zlib),
            3 => Ok(Compression::None),
            4 => Ok(Compression::Lz4),
            127 => bail!("chunk uses a custom compression algorithm (id 127), which is not read"),
            other => bail!("chunk uses an unknown compression id {other}"),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Compression::Gzip => "gzip",
            Compression::Zlib => "zlib",
            Compression::None => "none",
            Compression::Lz4 => "lz4",
        }
    }
}

/// Decompresses `data`, failing if the result would be longer than `max_out`
pub fn decompress(kind: Compression, data: &[u8], max_out: usize) -> Result<Vec<u8>> {
    match kind {
        Compression::None => {
            if data.len() > max_out {
                bail!(
                    "uncompressed chunk of {} bytes is over the {max_out} limit",
                    data.len()
                );
            }
            Ok(data.to_vec())
        }
        Compression::Gzip => read_limited(GzDecoder::new(data), max_out, "gzip"),
        Compression::Zlib => read_limited(ZlibDecoder::new(data), max_out, "zlib"),
        Compression::Lz4 => lz4_block_stream(data, max_out),
    }
}

fn read_limited(r: impl Read, max_out: usize, what: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    // one byte more than the limit tells "exactly the limit" from "over it"
    let n = r
        .take(max_out as u64 + 1)
        .read_to_end(&mut out)
        .with_context(|| format!("{what} data is damaged"))?;
    if n > max_out {
        bail!("{what} data inflates to more than {max_out} bytes");
    }
    Ok(out)
}

// ---- LZ4 ----

const LZ4_MAGIC: &[u8; 8] = b"LZ4Block";
const LZ4_HEADER: usize = 21;
const LZ4_SEED: u32 = 0x9747_B28C;
/// The block size lz4-java allows (32 MiB); a header that says more is not one of its blocks
const LZ4_MAX_BLOCK: usize = 1 << 25;

/// xxHash32, which the block stream uses as its checksum
pub fn xxh32(data: &[u8], seed: u32) -> u32 {
    const P1: u32 = 2_654_435_761;
    const P2: u32 = 2_246_822_519;
    const P3: u32 = 3_266_489_917;
    const P4: u32 = 668_265_263;
    const P5: u32 = 374_761_393;
    let rd = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    let mut rest = data;
    let mut h: u32;
    if data.len() >= 16 {
        let mut v = [
            seed.wrapping_add(P1).wrapping_add(P2),
            seed.wrapping_add(P2),
            seed,
            seed.wrapping_sub(P1),
        ];
        while rest.len() >= 16 {
            for (i, vi) in v.iter_mut().enumerate() {
                *vi = vi
                    .wrapping_add(rd(&rest[i * 4..]).wrapping_mul(P2))
                    .rotate_left(13)
                    .wrapping_mul(P1);
            }
            rest = &rest[16..];
        }
        h = v[0]
            .rotate_left(1)
            .wrapping_add(v[1].rotate_left(7))
            .wrapping_add(v[2].rotate_left(12))
            .wrapping_add(v[3].rotate_left(18));
    } else {
        h = seed.wrapping_add(P5);
    }
    h = h.wrapping_add(data.len() as u32);
    while rest.len() >= 4 {
        h = h
            .wrapping_add(rd(rest).wrapping_mul(P3))
            .rotate_left(17)
            .wrapping_mul(P4);
        rest = &rest[4..];
    }
    for &b in rest {
        h = h
            .wrapping_add((b as u32).wrapping_mul(P5))
            .rotate_left(11)
            .wrapping_mul(P1);
    }
    h ^= h >> 15;
    h = h.wrapping_mul(P2);
    h ^= h >> 13;
    h = h.wrapping_mul(P3);
    h ^= h >> 16;
    h
}

/// One LZ4 block (the format's sequences of literals and matches) into `out`, which must end at
/// exactly `expected` bytes
fn lz4_block(src: &[u8], out: &mut Vec<u8>, expected: usize) -> Result<()> {
    let start = out.len();
    let end = start
        .checked_add(expected)
        .context("LZ4 block length overflows")?;
    let mut i = 0usize;
    let len_ext = |i: &mut usize, mut n: usize| -> Result<usize> {
        loop {
            let b = *src.get(*i).context("LZ4 block ends inside a length")?;
            *i += 1;
            n = n.checked_add(b as usize).context("LZ4 length overflows")?;
            if b != 255 {
                return Ok(n);
            }
            if n > LZ4_MAX_BLOCK * 2 {
                bail!("LZ4 length is absurd");
            }
        }
    };
    while i < src.len() {
        let token = src[i];
        i += 1;
        let mut lit = (token >> 4) as usize;
        if lit == 15 {
            lit = len_ext(&mut i, lit)?;
        }
        if lit > src.len() - i || out.len() + lit > end {
            bail!("LZ4 literals run past the end of the block");
        }
        out.extend_from_slice(&src[i..i + lit]);
        i += lit;
        if i == src.len() {
            break; // the last sequence has literals only
        }
        if src.len() - i < 2 {
            bail!("LZ4 block ends inside an offset");
        }
        let off = u16::from_le_bytes([src[i], src[i + 1]]) as usize;
        i += 2;
        if off == 0 || off > out.len() - start {
            bail!("LZ4 match points before the start of the block");
        }
        let mut m = (token & 15) as usize;
        if m == 15 {
            m = len_ext(&mut i, m)?;
        }
        m += 4;
        if out.len() + m > end {
            bail!("LZ4 match runs past the end of the block");
        }
        // may overlap its own output, so a byte at a time
        let from = out.len() - off;
        for k in 0..m {
            let b = out[from + k];
            out.push(b);
        }
    }
    if out.len() != end {
        bail!(
            "LZ4 block decodes to {} bytes, its header says {expected}",
            out.len() - start
        );
    }
    Ok(())
}

fn lz4_block_stream(data: &[u8], max_out: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    loop {
        if pos == data.len() {
            // the writer ends the stream with an empty block; a stream cut at a block boundary is
            // read as far as it goes
            break;
        }
        if data.len() - pos < LZ4_HEADER {
            bail!("LZ4 stream ends inside a block header");
        }
        let h = &data[pos..pos + LZ4_HEADER];
        if &h[..8] != LZ4_MAGIC {
            bail!("LZ4 stream: a block does not start with `LZ4Block`");
        }
        let method = h[8] & 0xF0;
        let rd = |o: usize| i32::from_le_bytes([h[o], h[o + 1], h[o + 2], h[o + 3]]);
        let (clen, dlen, check) = (rd(9), rd(13), rd(17) as u32);
        if clen < 0 || dlen < 0 {
            bail!("LZ4 block with a negative length");
        }
        let (clen, dlen) = (clen as usize, dlen as usize);
        if dlen > LZ4_MAX_BLOCK || clen > LZ4_MAX_BLOCK + (LZ4_MAX_BLOCK >> 8) + 64 {
            bail!("LZ4 block of {dlen} bytes is over what the format allows");
        }
        pos += LZ4_HEADER;
        if dlen == 0 {
            break; // the end mark
        }
        if clen > data.len() - pos {
            bail!("LZ4 block runs past the end of the data");
        }
        if out.len() + dlen > max_out {
            bail!("LZ4 data inflates to more than {max_out} bytes");
        }
        let body = &data[pos..pos + clen];
        pos += clen;
        let at = out.len();
        match method {
            0x10 => {
                if clen != dlen {
                    bail!("LZ4 stored block of {clen} bytes says it holds {dlen}");
                }
                out.extend_from_slice(body);
            }
            0x20 => lz4_block(body, &mut out, dlen)?,
            other => bail!("LZ4 block with an unknown method {other:#x}"),
        }
        // the writer stores the xxHash32 of the block's bytes, seed 0x9747B28C, masked to its low 28
        // bits (lz4-java's `Checksum` view of the hash does that); the whole 32 bits are accepted too
        let sum = xxh32(&out[at..], LZ4_SEED);
        if sum != check && (sum & 0x0FFF_FFFF) != check {
            bail!("LZ4 block fails its checksum");
        }
    }
    Ok(out)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use flate2::write::{GzEncoder, ZlibEncoder};
    use flate2::Compression as Level;
    use std::io::Write;

    pub fn zlib(data: &[u8]) -> Vec<u8> {
        let mut e = ZlibEncoder::new(Vec::new(), Level::fast());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    pub fn gzip(data: &[u8]) -> Vec<u8> {
        let mut e = GzEncoder::new(Vec::new(), Level::fast());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    /// One LZ4Block of `data` stored raw (method 0x10), and the end mark
    pub fn lz4_stored(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in data.chunks(1000) {
            out.extend_from_slice(LZ4_MAGIC);
            out.push(0x10 | 9);
            out.extend_from_slice(&(chunk.len() as i32).to_le_bytes());
            out.extend_from_slice(&(chunk.len() as i32).to_le_bytes());
            out.extend_from_slice(&(xxh32(chunk, LZ4_SEED) & 0x0FFF_FFFF).to_le_bytes());
            out.extend_from_slice(chunk);
        }
        out.extend_from_slice(LZ4_MAGIC);
        out.push(0x10 | 9);
        out.extend_from_slice(&[0; 12]);
        out
    }

    // A real block made by the `lz4` tool (its frame minus the framing), of this 412-byte text
    fn sample() -> Vec<u8> {
        let mut d = b"minecraft:stone minecraft:stone minecraft:stone ".to_vec();
        d.extend(0u8..40);
        d.extend_from_slice(b"abcabcabcabcabcabcabcabc");
        d.extend(std::iter::repeat_n(b'X', 300));
        d
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }

    const BLOCK: &str = "ff016d696e6563726166743a73746f6e652010000dff1c000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20212223242526276162630300021f580100ff14505858585858";

    #[test]
    fn xxhash_matches_the_reference_values() {
        // the empty string with seed 0, and the content checksum the lz4 tool wrote for `sample`
        assert_eq!(xxh32(b"", 0), 0x02CC_5D05);
        assert_eq!(xxh32(&sample(), 0), 0xEF14_E0AF);
        // 1 to 3 trailing bytes, and a length under 16, are other paths of the algorithm
        assert_eq!(xxh32(b"a", 0), 0x550D_7456);
        assert_eq!(xxh32(b"abc", 0), 0x32D1_53FF);
    }

    #[test]
    fn a_real_lz4_block_decodes() {
        let want = sample();
        assert_eq!(want.len(), 412);
        let block = unhex(BLOCK);
        let mut out = Vec::new();
        lz4_block(&block, &mut out, 412).unwrap();
        assert_eq!(out, want);
        // and through the stream framing
        let mut s = Vec::new();
        s.extend_from_slice(LZ4_MAGIC);
        s.push(0x20 | 9);
        s.extend_from_slice(&(block.len() as i32).to_le_bytes());
        s.extend_from_slice(&412i32.to_le_bytes());
        s.extend_from_slice(&(xxh32(&want, LZ4_SEED) & 0x0FFF_FFFF).to_le_bytes());
        s.extend_from_slice(&block);
        s.extend_from_slice(LZ4_MAGIC);
        s.push(0x10 | 9);
        s.extend_from_slice(&[0; 12]);
        assert_eq!(decompress(Compression::Lz4, &s, 1 << 20).unwrap(), want);
    }

    #[test]
    fn lz4_damage_is_an_error() {
        let want = sample();
        let block = unhex(BLOCK);
        let body = &block[..];
        // every cut of the block, and every flipped byte, either errors or decodes to something
        // of the right length; none panics
        for n in 0..body.len() {
            let mut out = Vec::new();
            let _ = lz4_block(&body[..n], &mut out, 412);
        }
        for i in 0..body.len() {
            let mut b = body.to_vec();
            b[i] ^= 0xFF;
            let mut out = Vec::new();
            let _ = lz4_block(&b, &mut out, 412);
        }
        // a wrong declared length
        let mut out = Vec::new();
        assert!(lz4_block(body, &mut out, 411).is_err());
        let mut out = Vec::new();
        assert!(lz4_block(body, &mut out, 413).is_err());
        let _ = want;
        // a stored stream with a wrong checksum
        let mut s = lz4_stored(b"hello world");
        s[17] ^= 1;
        assert!(decompress(Compression::Lz4, &s, 100)
            .unwrap_err()
            .to_string()
            .contains("checksum"));
        // a stream that is not one
        assert!(decompress(Compression::Lz4, b"nope nope nope nope nope nope", 100).is_err());
        // a match before the start
        let mut out = Vec::new();
        assert!(lz4_block(&[0x04, 0x01, 0x00], &mut out, 4).is_err());
    }

    #[test]
    fn lz4_stored_blocks_round_trip_and_obey_the_limit() {
        let data: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
        let s = lz4_stored(&data);
        assert_eq!(decompress(Compression::Lz4, &s, 3000).unwrap(), data);
        assert!(decompress(Compression::Lz4, &s, 2999).is_err());
    }

    #[test]
    fn the_checksum_is_read_masked_or_whole() {
        let data = b"hello world, hello world".to_vec();
        let mut s = lz4_stored(&data);
        // the stored one is the low 28 bits; write the whole 32 and it still reads
        let full = xxh32(&data, LZ4_SEED);
        assert!(
            full > 0x0FFF_FFFF,
            "pick data whose hash has high bits for this test"
        );
        s[17..21].copy_from_slice(&full.to_le_bytes());
        assert_eq!(decompress(Compression::Lz4, &s, 100).unwrap(), data);
    }

    #[test]
    fn zlib_gzip_and_none_obey_the_limit() {
        let data = vec![7u8; 10_000];
        for (k, c) in [
            (Compression::Zlib, zlib(&data)),
            (Compression::Gzip, gzip(&data)),
            (Compression::None, data.clone()),
        ] {
            assert_eq!(decompress(k, &c, 10_000).unwrap(), data, "{}", k.name());
            assert!(decompress(k, &c, 9_999).is_err(), "{}", k.name());
        }
        // a bomb: 100 MB of zeros in a few hundred bytes stops at the limit
        let bomb = zlib(&vec![0u8; 100_000_000]);
        assert!(bomb.len() < 1_000_000);
        assert!(decompress(Compression::Zlib, &bomb, 1 << 20).is_err());
        assert!(decompress(Compression::Zlib, b"garbage", 100).is_err());
        assert!(decompress(Compression::Gzip, b"garbage", 100).is_err());
    }

    #[test]
    fn ids() {
        assert_eq!(Compression::from_id(2).unwrap(), Compression::Zlib);
        assert_eq!(Compression::from_id(4).unwrap(), Compression::Lz4);
        assert!(Compression::from_id(127)
            .unwrap_err()
            .to_string()
            .contains("custom"));
        assert!(Compression::from_id(0).is_err());
        assert!(Compression::from_id(9).is_err());
    }
}
